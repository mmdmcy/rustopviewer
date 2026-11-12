package main

import (
	"encoding/binary"
	"encoding/xml"
	"strings"
	"testing"
	"unicode/utf16"
)

// usLayout is a US keyboard: letters, digits, Shift for capitals and
// symbols, nothing for é or €.
func usLayout(r rune) layoutKey {
	switch {
	case r >= 'a' && r <= 'z':
		return layoutKey{vk: uint16(r - 'a' + 'A'), found: true}
	case r >= 'A' && r <= 'Z':
		return layoutKey{vk: uint16(r), shift: true, found: true}
	case r >= '0' && r <= '9':
		return layoutKey{vk: uint16(r), found: true}
	case r == '!':
		return layoutKey{vk: '1', shift: true, found: true}
	case r == '=':
		return layoutKey{vk: 0xbb, found: true}
	case r == '@': // as on a layout with @ on AltGr+Q
		return layoutKey{vk: 'Q', ctrl: true, alt: true, found: true}
	}
	return layoutKey{}
}

func TestWindowsKeysTypeWhatTheViewerSent(t *testing.T) {
	press := func(vk uint16) keyStroke { return keyStroke{vk: vk} }
	release := func(vk uint16) keyStroke { return keyStroke{vk: vk, up: true} }
	unit := func(u uint16, up bool) keyStroke { return keyStroke{unit: u, up: up} }
	type event struct {
		key         string
		down, shift bool
	}
	cases := []struct {
		name   string
		events []event
		want   []keyStroke
	}{
		{"a letter as its key", []event{{"a", true, false}, {"a", false, false}}, []keyStroke{press('A'), release('A')}},
		{"a capital without Shift held gets Shift for the press",
			[]event{{"A", true, false}, {"A", false, false}},
			[]keyStroke{press(vkShift), press('A'), release(vkShift), release('A')}},
		{"Shift held, then the capital, released lower-case",
			[]event{{"Shift", true, true}, {"A", true, true}, {"Shift", false, false}, {"a", false, false}},
			[]keyStroke{press(vkShift), press('A'), release(vkShift), release('A')}},
		{"Ctrl+C is the C key", []event{{"Control", true, false}, {"c", true, false}, {"c", false, false}, {"Control", false, false}},
			[]keyStroke{press(vkControl), press('C'), release('C'), release(vkControl)}},
		{"é is not on the layout: the character itself", []event{{"é", true, false}, {"é", false, false}},
			[]keyStroke{unit(0xe9, false), unit(0xe9, true)}},
		{"an emoji as its two UTF-16 units", []event{{"😀", true, false}},
			[]keyStroke{unit(0xd83d, false), unit(0xde00, false)}},
		{"AltGr's character is typed as itself, AltGr is not sent",
			[]event{{"AltGraph", true, false}, {"@", true, false}, {"@", false, false}, {"AltGraph", false, false}},
			[]keyStroke{unit('@', false), unit('@', true)}},
		{"Shift held where the layout needs none: the character itself",
			[]event{{"Shift", true, true}, {"=", true, true}},
			[]keyStroke{press(vkShift), unit('=', false)}},
		{"! from a viewer without Shift on the host's Shift+1",
			[]event{{"!", true, false}},
			[]keyStroke{press(vkShift), press('1'), release(vkShift)}},
		{"arrows are extended keys", []event{{"ArrowLeft", true, false}},
			[]keyStroke{{vk: 0x25, extended: true}}},
	}
	for _, c := range cases {
		k := winKeyboard{layout: usLayout}
		var got []keyStroke
		for _, e := range c.events {
			sym, ok := keysym(e.key)
			if !ok {
				t.Fatalf("%s: no keysym for %q", c.name, e.key)
			}
			got = append(got, k.plan(sym, e.down, e.shift)...)
		}
		if !equalStrokes(got, c.want) {
			t.Errorf("%s:\n got  %v\n want %v", c.name, got, c.want)
		}
	}
}

func equalStrokes(a, b []keyStroke) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func TestVkKeyScanAnswers(t *testing.T) {
	if k := parseVkKeyScan(0xffff); k.found {
		t.Error("-1 is no key")
	}
	if k := parseVkKeyScan(0x0141); !k.found || k.vk != 0x41 || !k.shift || k.ctrl || k.alt {
		t.Errorf("Shift+A: %+v", k)
	}
	if k := parseVkKeyScan(0x0651); !k.ctrl || !k.alt || k.shift {
		t.Errorf("AltGr+Q: %+v", k)
	}
	if k := parseVkKeyScan(0x0841); !k.otherShifts {
		t.Errorf("Hankaku: %+v", k)
	}
	for sym, want := range map[uint32]bool{'a': true, 0xe9: true, 0x0101f600: true, 0x7f: false, 0x9b: false, 0x0100d800: false, 0xff0d: false} {
		if _, ok := keyRune(sym); ok != want {
			t.Errorf("keyRune(%#x) = %v", sym, ok)
		}
	}
}

func TestAbsoluteCoordinatesLandOnTheirPixel(t *testing.T) {
	for _, size := range []int{1, 2, 800, 1080, 1920, 2560, 3840} {
		for _, pixel := range []int{0, 1, size / 2, size - 2, size - 1} {
			if pixel < 0 {
				continue
			}
			a := absolute(pixel, size)
			if a < 0 || a > 65535 {
				t.Fatalf("%d of %d: %d out of range", pixel, size, a)
			}
			if size > 1 && int(a)*size/65536 != pixel {
				t.Errorf("%d of %d: %d lands on %d", pixel, size, a, int(a)*size/65536)
			}
		}
	}
	if absolute(-5, 1920) != 0 || absolute(99999, 1920) != absolute(1919, 1920) {
		t.Error("off-screen points are not clamped")
	}
}

func TestSessionFileRoundTrip(t *testing.T) {
	key := strings.Repeat("ab", 32)
	k, a, err := parseSession(formatSession(key, "127.0.0.1:47800"))
	if err != nil || k != key || a != "127.0.0.1:47800" {
		t.Fatalf("%q %q %v", k, a, err)
	}
	for _, bad := range []string{"", "key=short\nlisten=127.0.0.1:1\n", "key=" + strings.Repeat("AB", 32) + "\nlisten=x\n", "key=" + key + "\n"} {
		if _, _, err := parseSession([]byte(bad)); err == nil {
			t.Errorf("%q should be refused", bad)
		}
	}
}

func TestTheTaskRunsOnlyAsTheSignedInUser(t *testing.T) {
	text := taskXML("S-1-5-21-1-2-3-1001", `C:\Users\R&D <x>\AppData\Local\rustopviewer\rustopviewer-host.exe`)
	var task struct {
		XMLName    xml.Name
		Triggers   *struct{} `xml:"Triggers"`
		Principals struct {
			Principal struct {
				UserID    string `xml:"UserId"`
				LogonType string
				RunLevel  string
			}
		}
		Settings struct {
			DisallowStartIfOnBatteries, StopIfGoingOnBatteries, AllowStartOnDemand bool
			ExecutionTimeLimit                                                     string
		}
		Actions struct {
			Exec struct{ Command, Arguments string }
		}
	}
	// encoding/xml does not switch encodings: read it as the UTF-8 it is here.
	if err := xml.Unmarshal([]byte(strings.Replace(text, `encoding="UTF-16"`, `encoding="UTF-8"`, 1)), &task); err != nil {
		t.Fatal(err)
	}
	p := task.Principals.Principal
	if task.XMLName.Local != "Task" || task.Triggers != nil || p.UserID != "S-1-5-21-1-2-3-1001" || p.LogonType != "InteractiveToken" || p.RunLevel != "LeastPrivilege" {
		t.Errorf("task: %+v", task)
	}
	s := task.Settings
	if s.DisallowStartIfOnBatteries || s.StopIfGoingOnBatteries || !s.AllowStartOnDemand || s.ExecutionTimeLimit != "PT0S" {
		t.Errorf("settings: %+v", s)
	}
	if task.Actions.Exec.Command != `C:\Users\R&D <x>\AppData\Local\rustopviewer\rustopviewer-host.exe` || task.Actions.Exec.Arguments != "host" {
		t.Errorf("action: %+v", task.Actions.Exec)
	}
	file := utf16File("<a>é</a>")
	if file[0] != 0xff || file[1] != 0xfe || binary.LittleEndian.Uint16(file[8:]) != 'é' {
		t.Errorf("UTF-16 file: % x", file)
	}
	for user, want := range map[string]string{`PC\alex`: "RustOp Viewer (alex)", `a/b:c`: "RustOp Viewer (a_b_c)", "jo.ann-1": "RustOp Viewer (jo.ann-1)"} {
		if got := taskName(user); got != want {
			t.Errorf("taskName(%q) = %q", user, got)
		}
	}
}

// fakePE is the start of a 64-bit console program: what windowless reads.
func fakePE(subsystem uint16) []byte {
	b := make([]byte, 0x200)
	b[0], b[1] = 'M', 'Z'
	binary.LittleEndian.PutUint32(b[0x3c:], 0x80)
	copy(b[0x80:], "PE\x00\x00")
	binary.LittleEndian.PutUint16(b[0x80+24:], 0x20b)
	binary.LittleEndian.PutUint16(b[0x80+24+68:], subsystem)
	return b
}

func TestWindowlessCopyChangesOnlyTheSubsystem(t *testing.T) {
	exe := fakePE(3)
	out, err := windowless(exe)
	if err != nil {
		t.Fatal(err)
	}
	if binary.LittleEndian.Uint16(out[0xdc:]) != 2 || binary.LittleEndian.Uint16(exe[0xdc:]) != 3 {
		t.Fatal("subsystem not patched in a copy")
	}
	diff := 0
	for i := range exe {
		if exe[i] != out[i] {
			diff++
		}
	}
	if diff != 1 {
		t.Errorf("%d bytes changed", diff)
	}
	for name, bad := range map[string][]byte{"ELF": []byte("\x7fELF" + strings.Repeat("\x00", 100)), "driver": fakePE(1), "short": fakePE(3)[:0x90]} {
		if _, err := windowless(bad); err == nil {
			t.Errorf("%s should be refused", name)
		}
	}
}

func TestClipboardTextFromWindows(t *testing.T) {
	enc := func(s string) []uint16 { return utf16.Encode([]rune(s)) }
	if text, big := fromUTF16(append(enc("a\r\nb 😀"), 0, 'x'), false); big || text != "a\nb 😀" {
		t.Errorf("got %q %v", text, big)
	}
	if text, big := fromUTF16(enc("no NUL"), false); big || text != "no NUL" {
		t.Errorf("got %q %v", text, big)
	}
	if _, big := fromUTF16(enc("cut short"), true); !big {
		t.Error("a block cut before its NUL is too big")
	}
	if _, big := fromUTF16(append(enc(strings.Repeat("é", maxClipboard/2+1)), 0), false); !big {
		t.Error("over 1 MB of UTF-8 is too big")
	}
	lines := strings.Repeat("\r\n", maxClipboard)
	if text, big := fromUTF16(append(enc(lines), 0), false); big || len(text) != maxClipboard {
		t.Errorf("1 MB of line ends: %d %v", len(text), big)
	}
	if got := windowsLines("a\nb\r\nc"); got != "a\r\nb\r\nc" {
		t.Errorf("windowsLines: %q", got)
	}
}
