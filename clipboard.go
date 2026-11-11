package main

// The X11 clipboard, on a connection of its own. To paste into the remote,
// RustOp Viewer owns the CLIPBOARD selection and hands its text to any
// program that asks (SelectionRequest for TARGETS, UTF8_STRING, TEXT or
// STRING). To copy from the remote, it asks the selection's owner for its
// text (ConvertSelection, then the property, in pieces when the owner uses
// INCR). Text only, at most maxClipboard bytes either way, never logged.

import (
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"time"
	"unicode/utf8"
)

// maxClipboard is the most text the clipboard carries in either direction.
const maxClipboard = 1 << 20

var (
	errClipboardTooBig  = errors.New("the clipboard holds more than 1 MB of text; RustOp Viewer carries up to 1 MB")
	errClipboardClosed  = errors.New("the connection to the X server's clipboard is closed")
	errClipboardTimeout = errors.New("the remote clipboard did not answer in time (the program holding it may be busy)")
)

// X11 event codes, predefined atoms and masks used here.
const (
	evPropertyNotify   = 28
	evSelectionClear   = 29
	evSelectionRequest = 30
	evSelectionNotify  = 31
	atomNone           = 0
	atomAtom           = 4
	atomString         = 31
	propertyNewValue   = 0
	propertyChangeMask = 0x400000
)

// X11Clipboard is the host's clipboard. One goroutine (run) does all of
// its X work, so requests, replies and events need no locks; callers hand
// it jobs.
type X11Clipboard struct {
	x       *X11
	window  uint32 // an unmapped input-only window that owns the selection
	atoms   struct{ clipboard, utf8, text, targets, incr, transfer uint32 }
	packets chan []byte // from the X server: replies, errors and events
	jobs    chan func()
	closed  chan struct{}
	backlog [][]byte // notifications that arrived during a round trip
	owned   bool     // we own CLIPBOARD and offer text
	text    []byte
	timeout time.Duration
}

// DialClipboard opens a second connection to a local display, for its
// clipboard.
func DialClipboard(display string) (*X11Clipboard, error) {
	x, err := connectX(display, 64<<10)
	if err != nil {
		return nil, err
	}
	c, err := newX11Clipboard(x)
	if err != nil {
		x.Close()
		return nil, err
	}
	return c, nil
}

func newX11Clipboard(x *X11) (*X11Clipboard, error) {
	c := &X11Clipboard{
		x:       x,
		packets: make(chan []byte, 16),
		jobs:    make(chan func()),
		closed:  make(chan struct{}),
		timeout: 5 * time.Second,
	}
	go c.readPackets()
	deadline := time.Now().Add(c.timeout)
	for _, a := range []struct {
		name string
		atom *uint32
	}{
		{"CLIPBOARD", &c.atoms.clipboard}, {"UTF8_STRING", &c.atoms.utf8}, {"TEXT", &c.atoms.text},
		{"TARGETS", &c.atoms.targets}, {"INCR", &c.atoms.incr}, {"RUSTOPVIEWER_CLIPBOARD", &c.atoms.transfer},
	} {
		reply, err := c.roundTrip(internAtom(a.name), deadline)
		if err != nil {
			return nil, err
		}
		*a.atom = le.Uint32(reply[8:])
	}
	// CreateWindow: depth and visual copied from the root, 1x1, no border,
	// InputOnly, told when its properties change (for INCR).
	c.window = x.idBase | x.idMask&-x.idMask
	create := request32(1, 0, c.window, x.Root, 0, 1|1<<16, 2<<16, 0, 0x800, propertyChangeMask)
	if err := c.send(create); err != nil {
		return nil, err
	}
	if _, err := c.roundTrip(request32(3, 0, c.window), deadline); err != nil { // GetWindowAttributes: it exists
		return nil, fmt.Errorf("clipboard window: %w", err)
	}
	go c.run()
	return c, nil
}

// SetText makes text the host's clipboard: we own CLIPBOARD and answer
// whoever pastes, until another program takes it over.
func (c *X11Clipboard) SetText(text string) error {
	if len(text) > maxClipboard {
		return errClipboardTooBig
	}
	data := []byte(strings.ReplaceAll(text, "\r\n", "\n")) // X11 text ends lines with \n
	return c.do(func() error {
		deadline := time.Now().Add(c.timeout)
		if err := c.send(request32(22, 0, c.window, c.atoms.clipboard, 0)); err != nil { // SetSelectionOwner
			return err
		}
		reply, err := c.roundTrip(request32(23, 0, c.atoms.clipboard), deadline) // GetSelectionOwner
		if err != nil {
			return err
		}
		if le.Uint32(reply[8:]) != c.window {
			c.owned, c.text = false, nil
			return errors.New("the X server did not hand the clipboard to RustOp Viewer")
		}
		c.owned, c.text = true, data
		return nil
	})
}

// Text returns the host clipboard's text: ours while we still own it,
// else what its owner hands over; "" when it holds no text.
func (c *X11Clipboard) Text() (string, error) {
	var text string
	err := c.do(func() (err error) {
		text, err = c.read()
		return err
	})
	return text, err
}

// Close ends the connection; the X server drops our selection with it.
func (c *X11Clipboard) Close() error { return c.x.Close() }

func (c *X11Clipboard) read() (string, error) {
	deadline := time.Now().Add(c.timeout)
	reply, err := c.roundTrip(request32(23, 0, c.atoms.clipboard), deadline)
	if err != nil {
		return "", err
	}
	switch le.Uint32(reply[8:]) {
	case atomNone:
		return "", nil
	case c.window:
		return string(c.text), nil
	}
	for _, target := range []uint32{c.atoms.utf8, atomString} {
		data, typ, err := c.convert(target, deadline)
		if err != nil {
			return "", err
		}
		var text string
		switch typ {
		case c.atoms.utf8, c.atoms.text:
			text = string(data)
			if !utf8.Valid(data) {
				text = strings.ToValidUTF8(text, "\uFFFD")
			}
		case atomString:
			text = fromLatin1(data)
		default:
			continue // refused, or not text (some owners answer anything with their image)
		}
		if len(text) > maxClipboard {
			return "", errClipboardTooBig
		}
		return text, nil
	}
	return "", nil // the owner holds something else (an image, files)
}

// convert asks the clipboard's owner for its text as target and reads it
// from our window. typ is None when the owner refused.
func (c *X11Clipboard) convert(target uint32, deadline time.Time) (data []byte, typ uint32, err error) {
	if err := c.send(request32(24, 0, c.window, c.atoms.clipboard, target, c.atoms.transfer, 0)); err != nil { // ConvertSelection
		return nil, 0, err
	}
	notify, err := c.waitEvent(func(p []byte) bool {
		return p[0]&0x7f == evSelectionNotify && le.Uint32(p[8:]) == c.window && le.Uint32(p[12:]) == c.atoms.clipboard
	}, deadline)
	if err != nil {
		return nil, 0, err
	}
	if le.Uint32(notify[20:]) == atomNone {
		return nil, atomNone, nil
	}
	typ, data, over, err := c.takeProperty(deadline)
	if err != nil || typ != c.atoms.incr {
		if err == nil && over {
			err = errClipboardTooBig
		}
		return data, typ, err
	}
	// INCR: every time we delete the property the owner writes the next
	// piece, and an empty piece ends it. Text over the limit is still read
	// to the end and dropped, so the owner finishes cleanly.
	var size uint32 // the owner's lower bound
	if len(data) >= 4 {
		size = le.Uint32(data)
	}
	tooBig := size > maxClipboard
	text := make([]byte, 0, min(size, maxClipboard))
	for {
		_, err := c.waitEvent(func(p []byte) bool {
			return p[0]&0x7f == evPropertyNotify && le.Uint32(p[4:]) == c.window &&
				le.Uint32(p[8:]) == c.atoms.transfer && p[16] == propertyNewValue
		}, deadline)
		if err != nil {
			return nil, 0, err
		}
		pieceType, piece, over, err := c.takeProperty(deadline)
		if err != nil {
			return nil, 0, err
		}
		if len(piece) == 0 && !over {
			break
		}
		typ = pieceType
		if over || len(text)+len(piece) > maxClipboard {
			tooBig, text = true, nil
		}
		if !tooBig {
			text = append(text, piece...)
		}
	}
	if tooBig {
		return nil, 0, errClipboardTooBig
	}
	return text, typ, nil
}

// takeProperty reads and deletes our window's transfer property: its
// type, its data (up to a little over maxClipboard) and whether it held
// more than maxClipboard.
func (c *X11Clipboard) takeProperty(deadline time.Time) (typ uint32, data []byte, over bool, err error) {
	reply, err := c.roundTrip(request32(20, 1, c.window, c.atoms.transfer, 0, 0, maxClipboard/4+1), deadline) // GetProperty, delete
	if err != nil {
		return 0, nil, false, err
	}
	n := int(le.Uint32(reply[16:])) * int(reply[1]/8)
	if n > len(reply)-32 {
		return 0, nil, false, errors.New("the X server sent a short property")
	}
	if le.Uint32(reply[12:]) > 0 { // bytes left: GetProperty deleted nothing
		over = true
		err = c.send(request32(19, 0, c.window, c.atoms.transfer)) // DeleteProperty
	}
	return le.Uint32(reply[8:]), reply[32 : 32+n], over || n > maxClipboard, err
}

// event serves what other programs ask of the clipboard's owner, and
// reports whether p was such an event.
func (c *X11Clipboard) event(p []byte) bool {
	switch p[0] & 0x7f {
	case evSelectionRequest:
		c.answer(p[4:28])
	case evSelectionClear: // another program copied: it owns the clipboard now
		if le.Uint32(p[12:]) == c.atoms.clipboard {
			c.owned, c.text = false, nil
		}
	default:
		return false
	}
	return true
}

// answer gives a program that pastes our text in the form it asked for,
// or refuses (property None), then tells it with a SelectionNotify.
func (c *X11Clipboard) answer(request []byte) {
	when, requestor, selection := le.Uint32(request[0:]), le.Uint32(request[8:]), le.Uint32(request[12:])
	target, property := le.Uint32(request[16:]), le.Uint32(request[20:])
	if property == atomNone { // an obsolete requestor: the target names the property
		property = target
	}
	if selection != c.atoms.clipboard || !c.owned || c.offer(requestor, property, target) != nil {
		property = atomNone
	}
	c.send(request32(25, 0, requestor, 0, evSelectionNotify, when, requestor, selection, target, property, 0, 0)) // SendEvent
}

func (c *X11Clipboard) offer(window, property, target uint32) error {
	switch target {
	case c.atoms.targets:
		list := make([]byte, 0, 16)
		for _, a := range []uint32{c.atoms.targets, c.atoms.utf8, c.atoms.text, atomString} {
			list = le.AppendUint32(list, a)
		}
		return c.changeProperty(window, property, atomAtom, 32, list)
	case c.atoms.utf8, c.atoms.text:
		return c.changeProperty(window, property, c.atoms.utf8, 8, c.text)
	case atomString:
		return c.changeProperty(window, property, atomString, 8, toLatin1(c.text))
	}
	return errors.New("not a text target")
}

// changeProperty sets a property in requests the server accepts: the
// first replaces, the rest append. The requestor reads it only after our
// SelectionNotify, which follows all of them.
func (c *X11Clipboard) changeProperty(window, property, typ uint32, format byte, data []byte) error {
	chunk := (max(c.x.maxRequest, 16384) - 24) &^ 3
	mode := byte(0) // Replace
	for {
		n := min(len(data), chunk)
		head := request32(18, mode, window, property, typ, uint32(format), uint32(n/int(format/8)))
		le.PutUint16(head[2:], uint16((24+n+pad4(n))/4))
		if err := c.send(head, data[:n], make([]byte, pad4(n))); err != nil {
			return err
		}
		if data = data[n:]; len(data) == 0 {
			return nil
		}
		mode = 2 // Append
	}
}

// run does the clipboard's X work: other programs' events while idle,
// and one job at a time.
func (c *X11Clipboard) run() {
	defer close(c.closed)
	for {
		select {
		case p, ok := <-c.packets:
			if !ok {
				return
			}
			c.event(p)
		case job := <-c.jobs:
			job()
			c.backlog = nil
		}
	}
}

func (c *X11Clipboard) do(job func() error) error {
	done := make(chan error, 1)
	select {
	case c.jobs <- func() { done <- job() }:
		return <-done
	case <-c.closed:
		return errClipboardClosed
	}
}

// readPackets passes every reply, error and event on to run, and closes
// the channel when the connection ends.
func (c *X11Clipboard) readPackets() {
	defer close(c.packets)
	for {
		head := make([]byte, 32)
		if _, err := io.ReadFull(c.x.r, head); err != nil {
			return
		}
		p := head
		if head[0] == 1 { // a reply: 32 bytes and its data
			p = make([]byte, 32+int(le.Uint32(head[4:]))*4)
			copy(p, head)
			if _, err := io.ReadFull(c.x.r, p[32:]); err != nil {
				return
			}
		}
		c.packets <- p
	}
}

func (c *X11Clipboard) send(parts ...[]byte) error {
	c.x.seq++
	bufs := net.Buffers(parts)
	_, err := bufs.WriteTo(c.x.conn)
	return err
}

// roundTrip sends a request and returns its reply, serving other programs'
// requests for our text meanwhile; notifications wait in the backlog.
func (c *X11Clipboard) roundTrip(b []byte, deadline time.Time) ([]byte, error) {
	if err := c.send(b); err != nil {
		return nil, err
	}
	seq := c.x.seq
	for {
		p, err := c.next(deadline)
		if err != nil {
			return nil, err
		}
		switch {
		case p[0] > 1:
			c.backlog = append(c.backlog, p)
		case le.Uint16(p[2:]) != seq:
			// for an earlier request: a requestor's window that went away
		case p[0] == 0:
			return nil, fmt.Errorf("X error %d for request %d", p[1], b[0])
		default:
			return p, nil
		}
	}
}

// waitEvent returns the first notification match accepts; the others are
// dropped.
func (c *X11Clipboard) waitEvent(match func([]byte) bool, deadline time.Time) ([]byte, error) {
	for len(c.backlog) > 0 {
		p := c.backlog[0]
		c.backlog = c.backlog[1:]
		if match(p) {
			return p, nil
		}
	}
	for {
		p, err := c.next(deadline)
		if err != nil {
			return nil, err
		}
		if p[0] > 1 && match(p) {
			return p, nil
		}
	}
}

// next returns the next reply, error or notification, serving selection
// requests on the way.
func (c *X11Clipboard) next(deadline time.Time) ([]byte, error) {
	timer := time.NewTimer(time.Until(deadline))
	defer timer.Stop()
	for {
		select {
		case p, ok := <-c.packets:
			if !ok {
				return nil, errClipboardClosed
			}
			if !c.event(p) {
				return p, nil
			}
		case <-timer.C:
			return nil, errClipboardTimeout
		}
	}
}

// request32 builds a request whose body is 32-bit values.
func request32(opcode, detail byte, values ...uint32) []byte {
	b := make([]byte, 4, 4+4*len(values))
	b[0], b[1] = opcode, detail
	le.PutUint16(b[2:], uint16(1+len(values)))
	for _, v := range values {
		b = le.AppendUint32(b, v)
	}
	return b
}

func internAtom(name string) []byte {
	b := make([]byte, 8+len(name)+pad4(len(name)))
	b[0] = 16
	le.PutUint16(b[2:], uint16(len(b)/4))
	le.PutUint16(b[4:], uint16(len(name)))
	copy(b[8:], name)
	return b
}

// toLatin1 gives text as STRING (ISO Latin-1): '?' for what it cannot hold.
func toLatin1(text []byte) []byte {
	out := make([]byte, 0, len(text))
	for _, r := range string(text) {
		if r > 0xff {
			r = '?'
		}
		out = append(out, byte(r))
	}
	return out
}

func fromLatin1(data []byte) string {
	out := make([]byte, 0, len(data))
	for _, b := range data {
		out = utf8.AppendRune(out, rune(b))
	}
	return string(out)
}
