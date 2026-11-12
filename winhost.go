package main

// The plain parts of the Windows host (host_windows.go), kept apart from
// its Win32 and Task Scheduler calls so they are tested on any system:
// the session file serve and the desktop's task share, the task's XML,
// the windowless copy of this program the task runs, and the clipboard's
// text conversions.

import (
	"bytes"
	"encoding/binary"
	"encoding/xml"
	"errors"
	"fmt"
	"strings"
	"unicode/utf16"
)

// A session file holds the session's key and the loopback address the
// desktop's side listens on; serve keeps it for as long as the session
// lasts, and the desktop's side stops when it is gone or changed.
func formatSession(key, address string) []byte {
	return []byte("key=" + key + "\nlisten=" + address + "\n")
}

func parseSession(data []byte) (key, address string, err error) {
	for _, line := range strings.Split(string(data), "\n") {
		k, v, _ := strings.Cut(strings.TrimSpace(line), "=")
		switch k {
		case "key":
			key = v
		case "listen":
			address = v
		}
	}
	if len(key) != 64 || strings.Trim(key, "0123456789abcdef") != "" || address == "" {
		return "", "", errors.New("the session file is not one serve wrote")
	}
	return key, address, nil
}

// taskName is the per-user task's name; Task Scheduler's names are shared
// by everyone on the PC.
func taskName(user string) string {
	if i := strings.LastIndexByte(user, '\\'); i >= 0 {
		user = user[i+1:]
	}
	clean := strings.Map(func(r rune) rune {
		if r == ' ' || r == '-' || r == '_' || r == '.' || r >= '0' && r <= '9' || r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z' {
			return r
		}
		return '_'
	}, user)
	return "RustOp Viewer (" + clean + ")"
}

// taskXML is the task: run as this user, only in their signed-in desktop
// session (InteractiveToken, no password stored), without administrator
// rights, no trigger (it starts only when serve asks), on battery too, for
// as long as the session lasts, one at a time.
func taskXML(userSID, command string) string {
	esc := func(s string) string {
		var b bytes.Buffer
		xml.EscapeText(&b, []byte(s))
		return b.String()
	}
	return `<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>RustOp Viewer: shows this desktop to an SSH session of this user that runs rustopviewer serve, while that session lasts. It has no trigger; rustopviewer uninstall removes it.</Description>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <UserId>` + esc(userSID) + `</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>` + esc(command) + `</Command>
      <Arguments>host</Arguments>
    </Exec>
  </Actions>
</Task>
`
}

// utf16File is text as UTF-16LE with a byte-order mark, as schtasks /xml
// reads it.
func utf16File(text string) []byte {
	out := []byte{0xff, 0xfe}
	for _, u := range utf16.Encode([]rune(text)) {
		out = binary.LittleEndian.AppendUint16(out, u)
	}
	return out
}

// windowless is a copy of a Windows console program marked as a GUI
// program (PE subsystem 2 instead of 3), the one difference `go build
// -ldflags -H=windowsgui` makes: started by Task Scheduler it opens no
// console window. Anything but a console or GUI PE file is refused.
func windowless(exe []byte) ([]byte, error) {
	le := binary.LittleEndian
	if len(exe) < 0x40 || exe[0] != 'M' || exe[1] != 'Z' {
		return nil, errors.New("not a Windows program")
	}
	pe := int(le.Uint32(exe[0x3c:]))
	subsystem := pe + 4 + 20 + 68 // signature, COFF header, then the optional header's field
	if pe < 0x40 || subsystem+2 > len(exe) || string(exe[pe:pe+4]) != "PE\x00\x00" {
		return nil, errors.New("not a Windows program")
	}
	if magic := le.Uint16(exe[pe+24:]); magic != 0x10b && magic != 0x20b {
		return nil, fmt.Errorf("unknown PE optional header %#x", magic)
	}
	switch le.Uint16(exe[subsystem:]) {
	case 2, 3:
	default:
		return nil, errors.New("neither a console nor a GUI program")
	}
	out := bytes.Clone(exe)
	le.PutUint16(out[subsystem:], 2)
	return out, nil
}

// windowsLines ends every line in CRLF.
func windowsLines(text string) string {
	return strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\n", "\r\n")
}

// fromUTF16 is clipboard memory as text up to its NUL, lines ending in LF;
// tooBig when it holds more than maxClipboard bytes of UTF-8. cut is true
// when units is only the start of a larger block.
func fromUTF16(units []uint16, cut bool) (text string, tooBig bool) {
	end := len(units)
	for i, u := range units {
		if u == 0 {
			end, cut = i, false
			break
		}
	}
	if cut {
		return "", true
	}
	text = strings.ReplaceAll(string(utf16.Decode(units[:end])), "\r\n", "\n")
	if len(text) > maxClipboard {
		return "", true
	}
	return text, false
}
