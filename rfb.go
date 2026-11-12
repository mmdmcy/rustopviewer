package main

// An RFB (VNC) client for a VNC server on this machine's loopback: on a
// Mac, macOS's own Screen Sharing on 127.0.0.1:5900. Screen Sharing already
// holds macOS's screen and input permissions, works at the lock screen and
// starts only when a connection comes, so RustOp Viewer needs no privacy
// permission of its own there. RFC 6143, only what a remote desktop needs:
// VNC authentication (security type 2), the 32-bit pixel format the X11
// host gives (BGRX), Raw and CopyRect updates, desktop size changes,
// pointer and key events (RFB keys are X keysyms, as keys.go makes them).

import (
	"bufio"
	"bytes"
	"crypto/des"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"time"
)

var rfbOrder = binary.BigEndian

const (
	rfbVNCAuth     = 2
	encRaw         = 0
	encCopyRect    = 1
	encDesktopSize = -223
	rfbMaxSide     = 16384
	rfbWait        = 10 * time.Second
)

var (
	errRFBNoPassword = errors.New("the VNC server takes no VNC password")
	errRFBRefused    = errors.New("the VNC server refused the password")
)

// RFB is a screen shown by a VNC server. Its methods are safe to call from
// several goroutines; a dropped connection is opened again by the next
// call, with the same password.
type RFB struct {
	addr, password string

	dial sync.Mutex // one (re)connect at a time
	in   sync.Mutex // input events go out in the order they were made
	mu   sync.Mutex // guards what follows
	c    *rfbConn   // nil until connected
	w, h int
	fb   []byte // the screen, BGRX
	snap []byte // the copy Capture returns
	full bool   // the next update request asks for the whole screen
	mask byte   // buttons held
	x, y int    // the pointer
	name string
}

// rfbConn is one connection, read by its own goroutine.
type rfbConn struct {
	net.Conn
	wmu     sync.Mutex
	pending bool          // an update request is out (RFB.mu)
	updated chan struct{} // an update arrived
	done    chan struct{} // closed when the connection ended, err says why
	err     error
}

func (c *rfbConn) send(b []byte) error {
	c.wmu.Lock()
	defer c.wmu.Unlock()
	c.SetWriteDeadline(time.Now().Add(rfbWait))
	_, err := c.Write(b)
	return err
}

// DialRFB connects to a VNC server on a loopback address with its VNC
// password.
func DialRFB(addr, password string) (*RFB, error) {
	host, _, err := net.SplitHostPort(addr)
	if err != nil || !isLoopbackHost(host) {
		return nil, fmt.Errorf("the VNC server must be on this machine's loopback, not %q", addr)
	}
	r := &RFB{addr: addr, password: password}
	if _, err := r.conn(); err != nil {
		return nil, err
	}
	return r, nil
}

// conn is the open connection, or a new one when the last one ended.
func (r *RFB) conn() (*rfbConn, error) {
	r.dial.Lock()
	defer r.dial.Unlock()
	r.mu.Lock()
	c := r.c
	r.mu.Unlock()
	if c != nil {
		select {
		case <-c.done:
		default:
			return c, nil
		}
	}
	conn, err := net.DialTimeout("tcp", r.addr, rfbWait)
	if err != nil {
		return nil, err
	}
	conn.SetDeadline(time.Now().Add(rfbWait))
	br := bufio.NewReaderSize(conn, 256<<10)
	w, h, name, err := rfbHandshake(conn, br, r.password)
	if err == nil {
		_, err = conn.Write(append(rfbPixelFormat(), rfbEncodings(encCopyRect, encRaw, encDesktopSize)...))
	}
	if err != nil {
		conn.Close()
		return nil, err
	}
	conn.SetDeadline(time.Time{})
	c = &rfbConn{Conn: conn, updated: make(chan struct{}, 1), done: make(chan struct{})}
	r.mu.Lock()
	r.c, r.name, r.full = c, name, true
	r.resize(w, h)
	r.mu.Unlock()
	go func() {
		c.err = r.read(c, br)
		c.Close()
		close(c.done)
	}()
	return c, nil
}

// rfbHandshake does RFB 3.8's opening up to ServerInit: version, VNC
// authentication, a shared session. It returns the screen's size and name.
func rfbHandshake(conn io.Writer, br *bufio.Reader, password string) (w, h int, name string, err error) {
	version := make([]byte, 12)
	if _, err = io.ReadFull(br, version); err != nil {
		return 0, 0, "", fmt.Errorf("the VNC server did not say hello: %w", err)
	}
	var major, minor int
	if n, _ := fmt.Sscanf(string(version), "RFB %03d.%03d\n", &major, &minor); n != 2 {
		return 0, 0, "", errors.New("the server on that port does not speak RFB (VNC)")
	}
	// 3.889 is Apple's 3.8.
	if major < 3 || major == 3 && minor < 8 {
		return 0, 0, "", fmt.Errorf("the VNC server speaks RFB %d.%d; RustOp Viewer needs 3.8", major, minor)
	}
	if _, err = io.WriteString(conn, "RFB 003.008\n"); err != nil {
		return 0, 0, "", err
	}
	count, err := br.ReadByte()
	if err != nil {
		return 0, 0, "", err
	}
	if count == 0 {
		return 0, 0, "", fmt.Errorf("the VNC server turned the connection away: %s", rfbReason(br))
	}
	types := make([]byte, count)
	if _, err = io.ReadFull(br, types); err != nil {
		return 0, 0, "", err
	}
	if bytes.IndexByte(types, rfbVNCAuth) < 0 {
		return 0, 0, "", fmt.Errorf("%w (it offers security types %v)", errRFBNoPassword, types)
	}
	if _, err = conn.Write([]byte{rfbVNCAuth}); err != nil {
		return 0, 0, "", err
	}
	challenge := make([]byte, 16)
	if _, err = io.ReadFull(br, challenge); err != nil {
		return 0, 0, "", err
	}
	if _, err = conn.Write(vncResponse(password, challenge)); err != nil {
		return 0, 0, "", err
	}
	result := make([]byte, 4)
	if _, err = io.ReadFull(br, result); err != nil {
		return 0, 0, "", err
	}
	if rfbOrder.Uint32(result) != 0 {
		return 0, 0, "", fmt.Errorf("%w (%s)", errRFBRefused, rfbReason(br))
	}
	if _, err = conn.Write([]byte{1}); err != nil { // ClientInit: share the session
		return 0, 0, "", err
	}
	init := make([]byte, 24)
	if _, err = io.ReadFull(br, init); err != nil {
		return 0, 0, "", err
	}
	w, h = int(rfbOrder.Uint16(init)), int(rfbOrder.Uint16(init[2:]))
	if w < 1 || h < 1 || w > rfbMaxSide || h > rfbMaxSide {
		return 0, 0, "", fmt.Errorf("the VNC server reports a %dx%d screen", w, h)
	}
	nameLen := int(rfbOrder.Uint32(init[20:]))
	if nameLen > 4096 {
		return 0, 0, "", errors.New("the VNC server sent a name too long to be one")
	}
	nameBytes := make([]byte, nameLen)
	if _, err = io.ReadFull(br, nameBytes); err != nil {
		return 0, 0, "", err
	}
	return w, h, string(nameBytes), nil
}

// rfbReason reads a server's reason string, as far as it makes sense.
func rfbReason(br *bufio.Reader) string {
	head := make([]byte, 4)
	if _, err := io.ReadFull(br, head); err != nil {
		return "no reason given"
	}
	n := min(int(rfbOrder.Uint32(head)), 1024)
	text := make([]byte, n)
	n, _ = io.ReadFull(br, text)
	if n == 0 {
		return "no reason given"
	}
	return strings.TrimSpace(string(text[:n]))
}

// vncResponse is VNC authentication's answer: the challenge encrypted with
// DES, the key the password's first 8 bytes with each byte's bits in
// reverse order (RFC 6143, 7.2.2, and every VNC server since).
func vncResponse(password string, challenge []byte) []byte {
	key := make([]byte, 8)
	copy(key, password)
	for i, b := range key {
		var r byte
		for bit := 0; bit < 8; bit++ {
			r |= (b >> bit & 1) << (7 - bit)
		}
		key[i] = r
	}
	block, _ := des.NewCipher(key) // 8 bytes: never fails
	out := make([]byte, 16)
	block.Encrypt(out, challenge[:8])
	block.Encrypt(out[8:], challenge[8:16])
	return out
}

// rfbPixelFormat asks for 32 bits per pixel, little-endian, red in bits
// 16-23, green 8-15, blue 0-7: in memory B, G, R, unused, as X11 gives.
func rfbPixelFormat() []byte {
	b := make([]byte, 20)
	b[0] = 0 // SetPixelFormat
	f := b[4:]
	f[0], f[1], f[2], f[3] = 32, 24, 0, 1
	rfbOrder.PutUint16(f[4:], 255)
	rfbOrder.PutUint16(f[6:], 255)
	rfbOrder.PutUint16(f[8:], 255)
	f[10], f[11], f[12] = 16, 8, 0
	return b
}

func rfbEncodings(list ...int32) []byte {
	b := make([]byte, 4, 4+4*len(list))
	b[0] = 2 // SetEncodings
	rfbOrder.PutUint16(b[2:], uint16(len(list)))
	for _, e := range list {
		b = rfbOrder.AppendUint32(b, uint32(e))
	}
	return b
}

func rfbUpdateRequest(incremental bool, w, h int) []byte {
	b := make([]byte, 10)
	b[0] = 3
	if incremental {
		b[1] = 1
	}
	rfbOrder.PutUint16(b[6:], uint16(w))
	rfbOrder.PutUint16(b[8:], uint16(h))
	return b
}

// resize makes an empty screen of w by h (r.mu held).
func (r *RFB) resize(w, h int) {
	r.w, r.h = w, h
	if cap(r.fb) >= w*h*4 {
		r.fb = r.fb[:w*h*4]
		clear(r.fb)
	} else {
		r.fb = make([]byte, w*h*4)
	}
	r.full = true
}

// read applies what the server sends until the connection ends.
func (r *RFB) read(c *rfbConn, br *bufio.Reader) error {
	head := make([]byte, 12)
	var rows []byte
	for {
		kind, err := br.ReadByte()
		if err != nil {
			return err
		}
		switch kind {
		case 0: // FramebufferUpdate
			if _, err := io.ReadFull(br, head[:3]); err != nil {
				return err
			}
			for n := int(rfbOrder.Uint16(head[1:])); n > 0; n-- {
				if _, err := io.ReadFull(br, head); err != nil {
					return err
				}
				x, y := int(rfbOrder.Uint16(head)), int(rfbOrder.Uint16(head[2:]))
				w, h := int(rfbOrder.Uint16(head[4:])), int(rfbOrder.Uint16(head[6:]))
				switch enc := int32(rfbOrder.Uint32(head[8:])); enc {
				case encRaw:
					if !r.fits(x, y, w, h) {
						return fmt.Errorf("the VNC server sent a rectangle outside the screen (%d,%d %dx%d)", x, y, w, h)
					}
					if cap(rows) < w*h*4 {
						rows = make([]byte, w*h*4)
					}
					rows = rows[:w*h*4]
					if _, err := io.ReadFull(br, rows); err != nil {
						return err
					}
					r.mu.Lock()
					for row := 0; row < h; row++ {
						copy(r.fb[((y+row)*r.w+x)*4:], rows[row*w*4:(row+1)*w*4])
					}
					r.mu.Unlock()
				case encCopyRect:
					if _, err := io.ReadFull(br, head[:4]); err != nil {
						return err
					}
					sx, sy := int(rfbOrder.Uint16(head)), int(rfbOrder.Uint16(head[2:]))
					if !r.fits(x, y, w, h) || !r.fits(sx, sy, w, h) {
						return fmt.Errorf("the VNC server copied a rectangle outside the screen")
					}
					r.copyRect(sx, sy, x, y, w, h)
				case encDesktopSize:
					if w < 1 || h < 1 || w > rfbMaxSide || h > rfbMaxSide {
						return fmt.Errorf("the VNC server reports a %dx%d screen", w, h)
					}
					r.mu.Lock()
					r.resize(w, h)
					r.mu.Unlock()
				default:
					return fmt.Errorf("the VNC server sent an update in encoding %d, which RustOp Viewer did not ask for", enc)
				}
			}
			r.mu.Lock()
			c.pending = false
			r.mu.Unlock()
			select {
			case c.updated <- struct{}{}:
			default:
			}
		case 1: // SetColourMapEntries: not for a true-colour format
			if _, err := io.ReadFull(br, head[:5]); err != nil {
				return err
			}
			if _, err := io.CopyN(io.Discard, br, int64(rfbOrder.Uint16(head[3:]))*6); err != nil {
				return err
			}
		case 2: // Bell
		case 3: // ServerCutText: the clipboard travels another way
			if _, err := io.ReadFull(br, head[:7]); err != nil {
				return err
			}
			if _, err := io.CopyN(io.Discard, br, int64(rfbOrder.Uint32(head[3:]))); err != nil {
				return err
			}
		default:
			return fmt.Errorf("the VNC server sent message type %d, which RustOp Viewer does not know", kind)
		}
	}
}

func (r *RFB) fits(x, y, w, h int) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return x+w <= r.w && y+h <= r.h
}

// copyRect copies a rectangle of the screen to another place on it, rows
// in the order that never reads a row already overwritten.
func (r *RFB) copyRect(sx, sy, x, y, w, h int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	for i := 0; i < h; i++ {
		row := i
		if sy < y {
			row = h - 1 - i
		}
		from := ((sy+row)*r.w + sx) * 4
		copy(r.fb[((y+row)*r.w+x)*4:], r.fb[from:from+w*4])
	}
}

// Size is the screen's size in pixels.
func (r *RFB) Size() (int, int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.w, r.h
}

// Capture asks for what changed and returns the screen once it came, or
// after a second without change. The slice is reused: it holds until the
// next Capture (the server encodes a frame before it asks for the next).
func (r *RFB) Capture() ([]byte, error) {
	c, err := r.conn()
	if err != nil {
		return nil, err
	}
	r.mu.Lock()
	ask := !c.pending
	request := rfbUpdateRequest(!r.full, r.w, r.h)
	if ask {
		c.pending, r.full = true, false
	}
	r.mu.Unlock()
	if ask {
		if err := c.send(request); err != nil {
			return nil, err
		}
	}
	select {
	case <-c.updated:
	case <-c.done:
		return nil, fmt.Errorf("the VNC server ended the connection: %w", c.err)
	case <-time.After(time.Second):
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.snap = append(r.snap[:0], r.fb...)
	return r.snap, nil
}

// pointer sends where the pointer is and which buttons are held.
func (r *RFB) pointer(update func()) error {
	r.in.Lock()
	defer r.in.Unlock()
	c, err := r.conn()
	if err != nil {
		return err
	}
	r.mu.Lock()
	update()
	b := []byte{5, r.mask, 0, 0, 0, 0}
	rfbOrder.PutUint16(b[2:], uint16(r.x))
	rfbOrder.PutUint16(b[4:], uint16(r.y))
	r.mu.Unlock()
	return c.send(b)
}

// Move puts the pointer at x, y.
func (r *RFB) Move(x, y int) error {
	return r.pointer(func() { r.x, r.y = clamp(x, r.w), clamp(y, r.h) })
}

// Button presses or releases button 1 (left), 2 (middle), 3 (right), 4/5
// (wheel up/down) or 6/7 (wheel left/right).
func (r *RFB) Button(button int, down bool) error {
	if button < 1 || button > 8 {
		return nil
	}
	bit := byte(1) << (button - 1)
	return r.pointer(func() {
		if down {
			r.mask |= bit
		} else {
			r.mask &^= bit
		}
	})
}

// Key presses or releases a key by its X keysym; the VNC server finds the
// key and any Shift it needs.
func (r *RFB) Key(sym uint32, down, _ bool) error {
	r.in.Lock()
	defer r.in.Unlock()
	c, err := r.conn()
	if err != nil {
		return err
	}
	b := []byte{4, 0, 0, 0, 0, 0, 0, 0}
	if down {
		b[1] = 1
	}
	rfbOrder.PutUint32(b[4:], sym)
	return c.send(b)
}

// Close ends the connection.
func (r *RFB) Close() error {
	r.mu.Lock()
	c := r.c
	r.mu.Unlock()
	if c == nil {
		return nil
	}
	return c.Close()
}
