package main

// The Windows clipboard as CF_UNICODETEXT: what the viewer pastes is put
// there (Windows keeps it, so it stays pasteable after RustOp Viewer
// ends), and what the host copied is read from there; Windows makes
// CF_UNICODETEXT of a program's CF_TEXT by itself. Text only, at most
// maxClipboard bytes of UTF-8 either way, never logged.

import (
	"errors"
	"runtime"
	"syscall"
	"time"
	"unicode/utf16"
	"unsafe"
)

var (
	procOpenClipboard              = user32.NewProc("OpenClipboard")
	procCloseClipboard             = user32.NewProc("CloseClipboard")
	procEmptyClipboard             = user32.NewProc("EmptyClipboard")
	procGetClipboardData           = user32.NewProc("GetClipboardData")
	procSetClipboardData           = user32.NewProc("SetClipboardData")
	procIsClipboardFormatAvailable = user32.NewProc("IsClipboardFormatAvailable")

	procGlobalAlloc   = kernel32.NewProc("GlobalAlloc")
	procGlobalFree    = kernel32.NewProc("GlobalFree")
	procGlobalLock    = kernel32.NewProc("GlobalLock")
	procGlobalUnlock  = kernel32.NewProc("GlobalUnlock")
	procGlobalSize    = kernel32.NewProc("GlobalSize")
	procRtlMoveMemory = kernel32.NewProc("RtlMoveMemory")
)

const (
	cfUnicodeText = 13
	gmemMoveable  = 0x0002
)

var errClipboardBusy = errors.New("another program holds the Windows clipboard open; try again")

type winClipboard struct{}

// withClipboard opens the clipboard for this thread (another program may
// hold it for a moment), runs f and closes it.
func withClipboard(f func() error) error {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	for try := 0; ; try++ {
		if r, _, _ := procOpenClipboard.Call(0); r != 0 {
			break
		}
		if try == 20 {
			return errClipboardBusy
		}
		time.Sleep(25 * time.Millisecond)
	}
	defer procCloseClipboard.Call()
	return f()
}

// SetText makes text the host's clipboard, its lines ending in CRLF as
// Windows programs expect.
func (winClipboard) SetText(text string) error {
	if len(text) > maxClipboard {
		return errClipboardTooBig
	}
	units := utf16.Encode([]rune(windowsLines(text)))
	units = append(units, 0)
	size := uintptr(len(units) * 2)
	return withClipboard(func() error {
		if r, _, err := procEmptyClipboard.Call(); r == 0 {
			return clipboardCallError("empty the clipboard", err)
		}
		h, _, err := procGlobalAlloc.Call(gmemMoveable, size)
		if h == 0 {
			return clipboardCallError("allocate the clipboard's memory", err)
		}
		p, _, err := procGlobalLock.Call(h)
		if p == 0 {
			procGlobalFree.Call(h)
			return clipboardCallError("lock the clipboard's memory", err)
		}
		procRtlMoveMemory.Call(p, uintptr(unsafe.Pointer(&units[0])), size)
		procGlobalUnlock.Call(h)
		if r, _, err := procSetClipboardData.Call(cfUnicodeText, h); r == 0 {
			procGlobalFree.Call(h) // still ours: Windows took nothing
			return clipboardCallError("set the clipboard", err)
		}
		return nil
	})
}

// Text returns the host clipboard's text with lines ending in LF; "" when
// it holds no text.
func (winClipboard) Text() (string, error) {
	var text string
	err := withClipboard(func() error {
		if r, _, _ := procIsClipboardFormatAvailable.Call(cfUnicodeText); r == 0 {
			return nil // empty, or an image or files
		}
		h, _, err := procGetClipboardData.Call(cfUnicodeText)
		if h == 0 {
			return clipboardCallError("read the clipboard", err)
		}
		p, _, err := procGlobalLock.Call(h)
		if p == 0 {
			return clipboardCallError("lock the clipboard's memory", err)
		}
		defer procGlobalUnlock.Call(h)
		size, _, _ := procGlobalSize.Call(h)
		// Each byte of the UTF-8 text is at most two of UTF-16, or four
		// for a line end (CR LF); a program's memory may run on past the
		// text's NUL.
		limit := uintptr(4*maxClipboard + 2)
		units := make([]uint16, min(size, limit)/2)
		if len(units) > 0 {
			procRtlMoveMemory.Call(uintptr(unsafe.Pointer(&units[0])), p, uintptr(len(units)*2))
		}
		var tooBig bool
		text, tooBig = fromUTF16(units, size > limit)
		if tooBig {
			return errClipboardTooBig
		}
		return nil
	})
	return text, err
}

func clipboardCallError(what string, err error) error {
	if errno, ok := err.(syscall.Errno); ok && errno != 0 {
		return errors.New("Windows could not " + what + ": " + errno.Error())
	}
	return errors.New("Windows could not " + what)
}
