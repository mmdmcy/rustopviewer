package main

// How a Windows host types the viewer's keys, kept apart from the Win32
// calls (screen_windows.go) so the choices are tested on any system.
//
// Named keys go out as their virtual-key codes. A character goes out as
// the key that types it on the host's keyboard layout when there is one
// (so Ctrl+C, Alt+F and Win+R work, and programs see real key presses),
// holding Shift for the press when the layout needs it; when the layout
// has no such key, or only with AltGr, or Shift is held where the layout
// needs none, it goes out as the character itself (KEYEVENTF_UNICODE), so
// any text types exactly, whatever layout either side uses.

import "unicode/utf16"

// Windows virtual-key codes used here.
const (
	vkShift   = 0xa0 // VK_LSHIFT
	vkControl = 0xa2 // VK_LCONTROL
	vkAlt     = 0xa4 // VK_LMENU
	vkWin     = 0x5b // VK_LWIN
)

type winKey struct {
	vk       uint16
	extended bool // KEYEVENTF_EXTENDEDKEY: the arrows, Insert, Delete, Home, End, ...
}

// winNamedKeys maps the X keysyms of keys.go's named keys to Windows keys.
// AltGraph maps to nothing: the viewer sends the character AltGr made.
var winNamedKeys = map[uint32]winKey{
	0xff0d: {0x0d, false}, 0xff08: {0x08, false}, 0xff09: {0x09, false}, 0xff1b: {0x1b, false},
	0xffff: {0x2e, true}, 0xff63: {0x2d, true}, 0xff50: {0x24, true}, 0xff57: {0x23, true},
	0xff55: {0x21, true}, 0xff56: {0x22, true},
	0xff51: {0x25, true}, 0xff52: {0x26, true}, 0xff53: {0x27, true}, 0xff54: {0x28, true},
	0xffe1: {vkShift, false}, 0xffe3: {vkControl, false}, 0xffe9: {vkAlt, false}, 0xfe03: {},
	0xffeb: {vkWin, true}, 0xffe5: {0x14, false}, 0xff67: {0x5d, true},
	0xff61: {0x2c, true}, 0xff14: {0x91, false}, 0xff13: {0x13, false}, 0xff7f: {0x90, true},
	0x0020: {0x20, false},
	0xffbe: {0x70, false}, 0xffbf: {0x71, false}, 0xffc0: {0x72, false}, 0xffc1: {0x73, false},
	0xffc2: {0x74, false}, 0xffc3: {0x75, false}, 0xffc4: {0x76, false}, 0xffc5: {0x77, false},
	0xffc6: {0x78, false}, 0xffc7: {0x79, false}, 0xffc8: {0x7a, false}, 0xffc9: {0x7b, false},
}

// keyStroke is one SendInput keyboard event: a virtual key, or (vk 0) one
// UTF-16 unit of a character.
type keyStroke struct {
	vk       uint16
	unit     uint16
	up       bool
	extended bool
}

// layoutKey is what VkKeyScanEx says types a character on the host's
// layout: the key, and the modifiers it needs.
type layoutKey struct {
	vk                 uint16
	shift, ctrl, alt   bool
	found, otherShifts bool // otherShifts: Hankaku or a layout's own shift states
}

// parseVkKeyScan reads VkKeyScanEx's answer: the key in the low byte, the
// shift state in the high byte, -1 for none.
func parseVkKeyScan(r uint16) layoutKey {
	if r == 0xffff || r&0xff == 0xff {
		return layoutKey{}
	}
	state := r >> 8
	return layoutKey{vk: r & 0xff, shift: state&1 != 0, ctrl: state&2 != 0, alt: state&4 != 0, found: true, otherShifts: state&^7 != 0}
}

// winKeyboard keeps what this host holds down, so every press gets its
// release and a shortcut is typed as one.
type winKeyboard struct {
	down map[uint16]bool
	// layout says which key types r on the host's current layout.
	layout func(r rune) layoutKey
}

// keyRune is the character an X keysym names, if it names one.
func keyRune(sym uint32) (rune, bool) {
	switch {
	case sym >= 0x20 && sym <= 0x7e, sym >= 0xa0 && sym <= 0xff:
		return rune(sym), true
	case sym&0xff000000 == 0x01000000:
		r := rune(sym & 0xffffff)
		return r, r >= 0x20 && r <= 0x10ffff && !(r >= 0xd800 && r <= 0xdfff)
	}
	return 0, false
}

// plan is what to send for one of the viewer's key events.
func (k *winKeyboard) plan(sym uint32, down, shiftHeld bool) []keyStroke {
	if k.down == nil {
		k.down = map[uint16]bool{}
	}
	if named, ok := winNamedKeys[sym]; ok {
		if named.vk == 0 {
			return nil
		}
		if down {
			k.down[named.vk] = true
		} else {
			delete(k.down, named.vk)
		}
		return []keyStroke{{vk: named.vk, extended: named.extended, up: !down}}
	}
	r, ok := keyRune(sym)
	if !ok {
		return nil
	}
	key := k.layout(r)
	if !down {
		if key.found && k.down[key.vk] {
			delete(k.down, key.vk)
			return []keyStroke{{vk: key.vk, up: true}}
		}
		return unicodeStrokes(r, true)
	}
	shiftHeld = shiftHeld || k.down[vkShift]
	shortcut := k.down[vkControl] || k.down[vkAlt] || k.down[vkWin]
	plain := key.found && !key.ctrl && !key.alt && !key.otherShifts
	if !plain || !shortcut && shiftHeld && !key.shift {
		return unicodeStrokes(r, false)
	}
	k.down[key.vk] = true
	press := keyStroke{vk: key.vk}
	if key.shift && !shiftHeld {
		return []keyStroke{{vk: vkShift}, press, {vk: vkShift, up: true}}
	}
	return []keyStroke{press}
}

// unicodeStrokes types r itself: one UTF-16 unit each, two for a
// character beyond the Basic Multilingual Plane.
func unicodeStrokes(r rune, up bool) []keyStroke {
	var out []keyStroke
	for _, u := range utf16.Encode([]rune{r}) {
		out = append(out, keyStroke{unit: u, up: up})
	}
	return out
}

// absolute turns a pixel into SendInput's absolute coordinate (0 to 65535
// across the primary monitor), rounded up so Windows' own rounding down
// lands on that pixel.
func absolute(pixel, size int) int32 {
	if size <= 1 {
		return 0
	}
	pixel = clamp(pixel, size)
	return int32(min((pixel*65536+size-1)/size, 65535))
}
