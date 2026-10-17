package main

import "unicode/utf8"

// namedKeys maps the browser's KeyboardEvent.key names to X keysyms.
var namedKeys = map[string]uint32{
	"Enter": 0xff0d, "Backspace": 0xff08, "Tab": 0xff09, "Escape": 0xff1b,
	"Delete": 0xffff, "Insert": 0xff63, "Home": 0xff50, "End": 0xff57,
	"PageUp": 0xff55, "PageDown": 0xff56,
	"ArrowLeft": 0xff51, "ArrowUp": 0xff52, "ArrowRight": 0xff53, "ArrowDown": 0xff54,
	"Shift": 0xffe1, "Control": 0xffe3, "Alt": 0xffe9, "AltGraph": 0xfe03,
	"Meta": 0xffeb, "OS": 0xffeb, "CapsLock": 0xffe5, "ContextMenu": 0xff67,
	"PrintScreen": 0xff61, "ScrollLock": 0xff14, "Pause": 0xff13, "NumLock": 0xff7f,
	" ":  0x0020,
	"F1": 0xffbe, "F2": 0xffbf, "F3": 0xffc0, "F4": 0xffc1, "F5": 0xffc2, "F6": 0xffc3,
	"F7": 0xffc4, "F8": 0xffc5, "F9": 0xffc6, "F10": 0xffc7, "F11": 0xffc8, "F12": 0xffc9,
}

// keysym returns the X keysym for a browser key name: a named key, or a
// single character (Latin-1 directly, other Unicode as 0x01000000+code).
func keysym(key string) (uint32, bool) {
	if sym, ok := namedKeys[key]; ok {
		return sym, true
	}
	r, size := utf8.DecodeRuneInString(key)
	if r == utf8.RuneError || size != len(key) || r < 0x20 || r == 0x7f {
		return 0, false
	}
	if r <= 0xff {
		return uint32(r), true
	}
	return 0x01000000 + uint32(r), true
}
