package main

// A minimal X11 client on the standard library: connect over the local
// socket with the MIT-MAGIC-COOKIE-1 from the Xauthority file, read the
// screen, take screenshots (GetImage) and send mouse and keyboard input
// through the XTEST extension. Only what a remote desktop needs.

import (
	"bufio"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
)

var le = binary.LittleEndian

// X11 is one connection to an X server. Its methods are safe to call from
// several goroutines.
type X11 struct {
	mu        sync.Mutex
	conn      net.Conn
	r         *bufio.Reader
	seq       uint16
	Root      uint32
	Width     int
	Height    int
	Depth     byte
	minCode   byte
	maxCode   byte
	xtest     byte              // XTEST major opcode, 0 when missing
	keys      map[uint32]keyRef // keysym -> keycode and whether Shift is needed
	byteOrder byte
}

type keyRef struct {
	code    byte
	shifted bool
}

// parseDisplay turns ":0" or ":1.0" into the display number.
func parseDisplay(display string) (int, error) {
	if !strings.HasPrefix(display, ":") {
		return 0, fmt.Errorf("only local displays like :0 are supported, not %q", display)
	}
	number := strings.SplitN(display[1:], ".", 2)[0]
	n, err := strconv.Atoi(number)
	if err != nil || n < 0 || n > 999 {
		return 0, fmt.Errorf("not a display: %q", display)
	}
	return n, nil
}

// xauthCookie finds the MIT-MAGIC-COOKIE-1 for a display in an Xauthority
// file: one for that display number first, else any.
func xauthCookie(data []byte, display int) ([]byte, error) {
	var any []byte
	want := strconv.Itoa(display)
	be := binary.BigEndian
	for len(data) >= 2 {
		data = data[2:] // family
		var fields [4][]byte
		for i := range fields {
			if len(data) < 2 {
				return nil, errors.New("Xauthority file is cut short")
			}
			n := int(be.Uint16(data))
			if len(data) < 2+n {
				return nil, errors.New("Xauthority file is cut short")
			}
			fields[i] = data[2 : 2+n]
			data = data[2+n:]
		}
		if string(fields[2]) != "MIT-MAGIC-COOKIE-1" {
			continue
		}
		if string(fields[1]) == want {
			return fields[3], nil
		}
		if any == nil {
			any = fields[3]
		}
	}
	if any == nil {
		return nil, errors.New("no MIT-MAGIC-COOKIE-1 in the Xauthority file")
	}
	return any, nil
}

func pad4(n int) int { return (4 - n%4) % 4 }

// setupRequest is the connection setup in little-endian byte order.
func setupRequest(cookie []byte) []byte {
	name := "MIT-MAGIC-COOKIE-1"
	if cookie == nil {
		name = ""
	}
	b := make([]byte, 12, 12+len(name)+pad4(len(name))+len(cookie)+pad4(len(cookie)))
	b[0] = 'l'
	le.PutUint16(b[2:], 11)
	le.PutUint16(b[6:], uint16(len(name)))
	le.PutUint16(b[8:], uint16(len(cookie)))
	b = append(b, name...)
	b = append(b, make([]byte, pad4(len(name)))...)
	b = append(b, cookie...)
	b = append(b, make([]byte, pad4(len(cookie)))...)
	return b
}

// parseSetup reads the server's setup reply data (after the 8-byte header)
// into the first screen.
func (x *X11) parseSetup(d []byte) error {
	if len(d) < 32 {
		return errors.New("X setup reply too short")
	}
	vendorLen := int(le.Uint16(d[16:]))
	screens := d[20]
	formats := int(d[21])
	x.byteOrder = d[22]
	x.minCode, x.maxCode = d[26], d[27]
	off := 32 + vendorLen + pad4(vendorLen) + 8*formats
	if screens == 0 || len(d) < off+40 {
		return errors.New("X server reports no screen")
	}
	s := d[off:]
	x.Root = le.Uint32(s[0:])
	x.Width = int(le.Uint16(s[20:]))
	x.Height = int(le.Uint16(s[22:]))
	x.Depth = s[38]
	return nil
}

// xauthPaths lists where display managers keep the X cookie: $XAUTHORITY,
// ~/.Xauthority, and the per-session files GDM and others use.
func xauthPaths() []string {
	var paths []string
	if p := os.Getenv("XAUTHORITY"); p != "" {
		paths = append(paths, p)
	}
	home, _ := os.UserHomeDir()
	paths = append(paths, filepath.Join(home, ".Xauthority"))
	runtime := os.Getenv("XDG_RUNTIME_DIR")
	if runtime == "" {
		runtime = fmt.Sprintf("/run/user/%d", os.Getuid())
	}
	return append(paths, filepath.Join(runtime, "gdm", "Xauthority"), filepath.Join(runtime, "Xauthority"))
}

// DialX11 connects to a local display (":0") with the cookie from
// $XAUTHORITY or ~/.Xauthority.
func DialX11(display string) (*X11, error) {
	n, err := parseDisplay(display)
	if err != nil {
		return nil, err
	}
	var cookie []byte
	for _, authPath := range xauthPaths() {
		data, err := os.ReadFile(authPath)
		if err != nil {
			continue
		}
		if cookie, err = xauthCookie(data, n); err == nil {
			break
		}
	}
	conn, err := net.Dial("unix", fmt.Sprintf("/tmp/.X11-unix/X%d", n))
	if err != nil {
		return nil, fmt.Errorf("connect to display %s: %w", display, err)
	}
	x := &X11{conn: conn, r: bufio.NewReaderSize(conn, 1<<20)}
	if err := x.setup(cookie); err != nil {
		conn.Close()
		return nil, err
	}
	return x, nil
}

func (x *X11) setup(cookie []byte) error {
	if _, err := x.conn.Write(setupRequest(cookie)); err != nil {
		return err
	}
	head := make([]byte, 8)
	if _, err := io.ReadFull(x.r, head); err != nil {
		return err
	}
	data := make([]byte, int(le.Uint16(head[6:]))*4)
	if _, err := io.ReadFull(x.r, data); err != nil {
		return err
	}
	if head[0] != 1 {
		reason := data
		if int(head[1]) <= len(reason) {
			reason = reason[:head[1]]
		}
		return fmt.Errorf("X server refused the connection: %s", strings.TrimSpace(string(reason)))
	}
	if err := x.parseSetup(data); err != nil {
		return err
	}
	if x.byteOrder != 0 {
		return errors.New("only little-endian X image formats are supported")
	}
	major, err := x.queryExtension("XTEST")
	if err != nil {
		return err
	}
	x.xtest = major
	return x.loadKeyboard()
}

// request sends one request and, when it has a reply, returns the reply
// (header and data). Events and errors for other requests are skipped.
func (x *X11) request(b []byte, wantReply bool) ([]byte, error) {
	x.seq++
	seq := x.seq
	if _, err := x.conn.Write(b); err != nil {
		return nil, err
	}
	if !wantReply {
		return nil, nil
	}
	for {
		head := make([]byte, 32)
		if _, err := io.ReadFull(x.r, head); err != nil {
			return nil, err
		}
		switch head[0] {
		case 0: // error
			if le.Uint16(head[2:]) == seq {
				return nil, fmt.Errorf("X error %d for request %d", head[1], b[0])
			}
		case 1: // reply
			extra := int(le.Uint32(head[4:])) * 4
			reply := make([]byte, 32+extra)
			copy(reply, head)
			if _, err := io.ReadFull(x.r, reply[32:]); err != nil {
				return nil, err
			}
			if le.Uint16(head[2:]) == seq {
				return reply, nil
			}
		}
		// Anything else is an event; nothing here asks for events.
	}
}

func (x *X11) queryExtension(name string) (byte, error) {
	n := len(name)
	b := make([]byte, 8+n+pad4(n))
	b[0] = 98
	le.PutUint16(b[2:], uint16(len(b)/4))
	le.PutUint16(b[4:], uint16(n))
	copy(b[8:], name)
	reply, err := x.request(b, true)
	if err != nil {
		return 0, err
	}
	if reply[8] == 0 {
		return 0, fmt.Errorf("the X server has no %s extension (needed for mouse and keyboard)", name)
	}
	return reply[9], nil
}

func (x *X11) loadKeyboard() error {
	count := int(x.maxCode) - int(x.minCode) + 1
	b := make([]byte, 8)
	b[0] = 101
	le.PutUint16(b[2:], 2)
	b[4] = x.minCode
	b[5] = byte(count)
	reply, err := x.request(b, true)
	if err != nil {
		return err
	}
	x.keys = keyMap(reply[32:], int(reply[1]), x.minCode, count)
	return nil
}

// keyMap indexes GetKeyboardMapping data: for each keysym the first
// keycode that has it, unshifted (column 0) before shifted (column 1).
func keyMap(data []byte, perCode int, minCode byte, count int) map[uint32]keyRef {
	keys := map[uint32]keyRef{}
	for column := 0; column < 2 && column < perCode; column++ {
		for i := 0; i < count; i++ {
			at := (i*perCode + column) * 4
			if at+4 > len(data) {
				break
			}
			sym := le.Uint32(data[at:])
			if _, seen := keys[sym]; sym != 0 && !seen {
				keys[sym] = keyRef{code: minCode + byte(i), shifted: column == 1}
			}
		}
	}
	return keys
}

// Capture returns the whole screen as BGRX bytes, 4 per pixel.
func (x *X11) Capture() ([]byte, error) {
	x.mu.Lock()
	defer x.mu.Unlock()
	b := make([]byte, 20)
	b[0] = 73
	b[1] = 2 // ZPixmap
	le.PutUint16(b[2:], 5)
	le.PutUint32(b[4:], x.Root)
	le.PutUint16(b[12:], uint16(x.Width))
	le.PutUint16(b[14:], uint16(x.Height))
	le.PutUint32(b[16:], 0xffffffff)
	reply, err := x.request(b, true)
	if err != nil {
		return nil, err
	}
	data := reply[32:]
	if len(data) < x.Width*x.Height*4 {
		return nil, fmt.Errorf("screen depth %d is not 24/32-bit", reply[1])
	}
	return data[:x.Width*x.Height*4], nil
}

// fakeInput sends one XTEST FakeInput: type 2/3 key press/release, 4/5
// button press/release, 6 motion (detail 0, absolute x, y).
func (x *X11) fakeInput(kind, detail byte, px, py int) error {
	b := make([]byte, 36)
	b[0] = x.xtest
	b[1] = 2
	le.PutUint16(b[2:], 9)
	b[4] = kind
	b[5] = detail
	le.PutUint32(b[12:], x.Root)
	le.PutUint16(b[24:], uint16(int16(px)))
	le.PutUint16(b[26:], uint16(int16(py)))
	_, err := x.request(b, false)
	return err
}

// Move puts the pointer at x, y.
func (x *X11) Move(px, py int) error {
	x.mu.Lock()
	defer x.mu.Unlock()
	return x.fakeInput(6, 0, clamp(px, x.Width), clamp(py, x.Height))
}

// Button presses or releases button 1 (left), 2 (middle), 3 (right) or
// 4/5 (wheel up/down).
func (x *X11) Button(button int, down bool) error {
	if button < 1 || button > 7 {
		return nil
	}
	x.mu.Lock()
	defer x.mu.Unlock()
	kind := byte(5)
	if down {
		kind = 4
	}
	return x.fakeInput(kind, byte(button), 0, 0)
}

const keysymShift = 0xffe1

// Key presses or releases the key for a keysym, holding Shift for a symbol
// that only sits on a shifted key (unless Shift is held already).
func (x *X11) Key(sym uint32, down, shiftHeld bool) error {
	x.mu.Lock()
	defer x.mu.Unlock()
	ref, ok := x.keys[sym]
	if !ok {
		return nil // nothing on this keyboard types it
	}
	kind := byte(3)
	if down {
		kind = 2
	}
	shift, hasShift := x.keys[keysymShift]
	wrap := ref.shifted && !shiftHeld && hasShift
	if wrap && down {
		if err := x.fakeInput(2, shift.code, 0, 0); err != nil {
			return err
		}
	}
	err := x.fakeInput(kind, ref.code, 0, 0)
	if wrap && !down {
		if err2 := x.fakeInput(3, shift.code, 0, 0); err == nil {
			err = err2
		}
	}
	return err
}

func clamp(v, size int) int {
	if v < 0 {
		return 0
	}
	if v >= size {
		return size - 1
	}
	return v
}

// Close ends the connection.
func (x *X11) Close() error { return x.conn.Close() }
