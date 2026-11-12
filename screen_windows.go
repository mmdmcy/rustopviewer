package main

// A Windows desktop through Windows' own DLLs, no third-party code: the
// primary monitor read with GDI (BitBlt into a bitmap, GetDIBits out of
// it), mouse and keyboard through SendInput. It must run in the signed-in
// user's desktop session; over SSH, serve starts it there (session_windows.go).

import (
	"errors"
	"fmt"
	"sync"
	"syscall"
	"unsafe"
)

var (
	user32   = syscall.NewLazyDLL("user32.dll")
	gdi32    = syscall.NewLazyDLL("gdi32.dll")
	kernel32 = syscall.NewLazyDLL("kernel32.dll")

	procSetProcessDpiAwarenessContext = user32.NewProc("SetProcessDpiAwarenessContext")
	procSetProcessDPIAware            = user32.NewProc("SetProcessDPIAware")
	procGetSystemMetrics              = user32.NewProc("GetSystemMetrics")
	procGetDC                         = user32.NewProc("GetDC")
	procReleaseDC                     = user32.NewProc("ReleaseDC")
	procSendInput                     = user32.NewProc("SendInput")
	procVkKeyScanExW                  = user32.NewProc("VkKeyScanExW")
	procMapVirtualKeyW                = user32.NewProc("MapVirtualKeyW")
	procGetKeyboardLayout             = user32.NewProc("GetKeyboardLayout")
	procGetForegroundWindow           = user32.NewProc("GetForegroundWindow")
	procGetWindowThreadProcessId      = user32.NewProc("GetWindowThreadProcessId")
	procGetProcessWindowStation       = user32.NewProc("GetProcessWindowStation")
	procGetUserObjectInformationW     = user32.NewProc("GetUserObjectInformationW")

	procCreateCompatibleDC     = gdi32.NewProc("CreateCompatibleDC")
	procCreateCompatibleBitmap = gdi32.NewProc("CreateCompatibleBitmap")
	procSelectObject           = gdi32.NewProc("SelectObject")
	procBitBlt                 = gdi32.NewProc("BitBlt")
	procGetDIBits              = gdi32.NewProc("GetDIBits")
	procDeleteObject           = gdi32.NewProc("DeleteObject")
	procDeleteDC               = gdi32.NewProc("DeleteDC")
)

const (
	smCXScreen          = 0
	smCYScreen          = 1
	srcCopy             = 0x00cc0020
	captureBlt          = 0x40000000 // layered windows too (tooltips, some menus)
	dibRGBColors        = 0
	inputMouse          = 0
	inputKeyboard       = 1
	mouseMove           = 0x0001
	mouseAbsolute       = 0x8000
	mouseWheel          = 0x0800
	mouseHWheel         = 0x1000
	wheelDelta          = 120
	keyExtended         = 0x0001
	keyUp               = 0x0002
	keyUnicode          = 0x0004
	uoiFlags            = 1
	wsfVisible          = 1
	perMonitorAwareV2   = ^uintptr(3) // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, (HANDLE)-4
	mapVirtualKeyToScan = 0
)

// mouseInput and keybdInput are MOUSEINPUT and KEYBDINPUT; input is INPUT,
// whose union is as large as MOUSEINPUT and aligned like it.
type mouseInput struct {
	dx, dy                 int32
	mouseData, flags, time uint32
	extraInfo              uintptr
}

type keybdInput struct {
	vk, scan    uint16
	flags, time uint32
	extraInfo   uintptr
}

type input struct {
	kind uint32
	mi   mouseInput
}

type bitmapInfo struct {
	size                 uint32
	width, height        int32
	planes, bitCount     uint16
	compression, sizeImg uint32
	xPels, yPels         int32
	clrUsed, clrImp      uint32
	colors               [1]uint32
}

// setDPIAware makes Windows give this process real pixels: per-monitor
// aware (Windows 10 1703 and later), else system aware. Without it a
// scaled display (125%, 150%) is read shrunk and clicks land off.
func setDPIAware() {
	if procSetProcessDpiAwarenessContext.Find() == nil {
		if r, _, _ := procSetProcessDpiAwarenessContext.Call(perMonitorAwareV2); r != 0 {
			return
		}
	}
	if procSetProcessDPIAware.Find() == nil {
		procSetProcessDPIAware.Call()
	}
}

// desktopVisible says whether this process's window station is the
// interactive one, as .NET's Environment.UserInteractive reads it: false
// in session 0, where services and SSH sessions run.
func desktopVisible() bool {
	station, _, _ := procGetProcessWindowStation.Call()
	if station == 0 {
		return false
	}
	var flags struct {
		inherit, reserved uint32
		flags             uint32
	}
	var needed uint32
	r, _, _ := procGetUserObjectInformationW.Call(station, uoiFlags, uintptr(unsafe.Pointer(&flags)), unsafe.Sizeof(flags), uintptr(unsafe.Pointer(&needed)))
	return r != 0 && flags.flags&wsfVisible != 0
}

// winScreen is the primary monitor, its mouse and keyboard.
type winScreen struct {
	mu            sync.Mutex
	width, height int
	memDC, bitmap uintptr
	pixels        []byte
	keys          winKeyboard
}

func openWinScreen() (*winScreen, error) {
	setDPIAware()
	s := &winScreen{}
	s.keys.layout = foregroundLayoutKey
	if err := s.size(); err != nil {
		return nil, err
	}
	return s, nil
}

func (s *winScreen) size() error {
	w, _, _ := procGetSystemMetrics.Call(smCXScreen)
	h, _, _ := procGetSystemMetrics.Call(smCYScreen)
	if int32(w) <= 0 || int32(h) <= 0 {
		return errors.New("Windows reports no screen for this session")
	}
	s.width, s.height = int(int32(w)), int(int32(h))
	return nil
}

func (s *winScreen) Size() (int, int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.width, s.height
}

// errScreenUnreadable is what Windows' refusal to read the screen means
// almost always.
var errScreenUnreadable = errors.New("Windows did not let RustOp Viewer read the screen: it is locked, or a secure desktop (a UAC prompt, Ctrl+Alt+Del) is up")

// Capture reads the primary monitor as BGRX, 4 bytes per pixel, into a
// buffer that is reused: it is valid until the next Capture.
func (s *winScreen) Capture() ([]byte, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	w, h := s.width, s.height
	if err := s.size(); err != nil {
		return nil, err
	}
	screen, _, _ := procGetDC.Call(0)
	if screen == 0 {
		return nil, errScreenUnreadable
	}
	defer procReleaseDC.Call(0, screen)
	if s.memDC == 0 {
		if s.memDC, _, _ = procCreateCompatibleDC.Call(screen); s.memDC == 0 {
			return nil, errors.New("Windows could not make a drawing context to copy the screen into")
		}
	}
	if s.bitmap == 0 || w != s.width || h != s.height {
		if s.bitmap != 0 {
			procDeleteObject.Call(s.bitmap)
		}
		if s.bitmap, _, _ = procCreateCompatibleBitmap.Call(screen, uintptr(s.width), uintptr(s.height)); s.bitmap == 0 {
			return nil, fmt.Errorf("Windows could not make a %dx%d bitmap to copy the screen into", s.width, s.height)
		}
		s.pixels = make([]byte, s.width*s.height*4)
	}
	old, _, _ := procSelectObject.Call(s.memDC, s.bitmap)
	copied, _, _ := procBitBlt.Call(s.memDC, 0, 0, uintptr(s.width), uintptr(s.height), screen, 0, 0, srcCopy|captureBlt)
	procSelectObject.Call(s.memDC, old) // GetDIBits wants the bitmap selected nowhere
	if copied == 0 {
		return nil, errScreenUnreadable
	}
	info := bitmapInfo{width: int32(s.width), height: -int32(s.height), planes: 1, bitCount: 32}
	info.size = uint32(unsafe.Offsetof(info.colors))
	lines, _, _ := procGetDIBits.Call(s.memDC, s.bitmap, 0, uintptr(s.height), uintptr(unsafe.Pointer(&s.pixels[0])), uintptr(unsafe.Pointer(&info)), dibRGBColors)
	if int(lines) != s.height {
		return nil, errors.New("Windows did not hand over the screen's pixels")
	}
	return s.pixels, nil
}

// Close frees the bitmap and its drawing context.
func (s *winScreen) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.bitmap != 0 {
		procDeleteObject.Call(s.bitmap)
	}
	if s.memDC != 0 {
		procDeleteDC.Call(s.memDC)
	}
	s.bitmap, s.memDC = 0, 0
}

func sendInputs(inputs []input) error {
	if len(inputs) == 0 {
		return nil
	}
	n, _, err := procSendInput.Call(uintptr(len(inputs)), uintptr(unsafe.Pointer(&inputs[0])), unsafe.Sizeof(inputs[0]))
	if int(n) != len(inputs) {
		// UIPI: a window running as administrator takes no input from a
		// process that is not.
		return fmt.Errorf("Windows took %d of %d input events (a window running as administrator refuses input from RustOp Viewer, and the lock screen takes none): %v", n, len(inputs), err)
	}
	return nil
}

func mouseEvent(flags, data uint32, dx, dy int32) input {
	return input{kind: inputMouse, mi: mouseInput{dx: dx, dy: dy, mouseData: data, flags: flags}}
}

// Move puts the pointer at x, y on the primary monitor.
func (s *winScreen) Move(x, y int) error {
	w, h := s.Size()
	return sendInputs([]input{mouseEvent(mouseMove|mouseAbsolute, 0, absolute(x, w), absolute(y, h))})
}

// Button presses or releases button 1 (left), 2 (middle) or 3 (right);
// a press of 4/5 turns the wheel up/down one notch, 6/7 left/right.
func (s *winScreen) Button(button int, down bool) error {
	var flags uint32
	var data int32
	switch button {
	case 1, 2, 3:
		// LEFTDOWN, MIDDLEDOWN, RIGHTDOWN; each one's UP is twice its DOWN.
		flags = [4]uint32{0, 0x02, 0x20, 0x08}[button]
		if !down {
			flags *= 2
		}
	case 4, 5, 6, 7:
		if !down {
			return nil
		}
		// Up and right are positive notches.
		flags, data = mouseWheel, wheelDelta
		if button >= 6 {
			flags = mouseHWheel
		}
		if button == 5 || button == 6 {
			data = -wheelDelta
		}
	default:
		return nil
	}
	return sendInputs([]input{mouseEvent(flags, uint32(data), 0, 0)})
}

// Key types one of the viewer's key events (winkeys.go decides how).
func (s *winScreen) Key(sym uint32, down, shiftHeld bool) error {
	s.mu.Lock()
	strokes := s.keys.plan(sym, down, shiftHeld)
	s.mu.Unlock()
	inputs := make([]input, len(strokes))
	for i, k := range strokes {
		inputs[i].kind = inputKeyboard
		ki := (*keybdInput)(unsafe.Pointer(&inputs[i].mi))
		switch {
		case k.vk == 0:
			ki.scan, ki.flags = k.unit, keyUnicode
		default:
			scan, _, _ := procMapVirtualKeyW.Call(uintptr(k.vk), mapVirtualKeyToScan)
			ki.vk, ki.scan = k.vk, uint16(scan)
			if k.extended {
				ki.flags |= keyExtended
			}
		}
		if k.up {
			ki.flags |= keyUp
		}
	}
	return sendInputs(inputs)
}

// foregroundLayoutKey asks the keyboard layout of the window that has the
// focus (each window can have its own) which key types r.
func foregroundLayoutKey(r rune) layoutKey {
	if r > 0xffff {
		return layoutKey{}
	}
	window, _, _ := procGetForegroundWindow.Call()
	thread, _, _ := procGetWindowThreadProcessId.Call(window, 0)
	layout, _, _ := procGetKeyboardLayout.Call(thread)
	res, _, _ := procVkKeyScanExW.Call(uintptr(r), layout)
	return parseVkKeyScan(uint16(res))
}
