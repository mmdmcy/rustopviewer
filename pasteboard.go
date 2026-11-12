package main

// A clipboard through two commands that read and write it as text: on a
// Mac, macOS's own pbpaste and pbcopy, which reach the signed-in person's
// pasteboard from an SSH session too. Text only, at most maxClipboard
// bytes either way, never logged.

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
	"unicode/utf8"
)

// Pasteboard runs Paste to read the clipboard and Copy, with the text on
// its input, to set it.
type Pasteboard struct {
	Paste, Copy []string
	Env         []string // added to the environment: the commands' text encoding
}

// macPasteboard is macOS's own; LANG makes both speak UTF-8.
var macPasteboard = Pasteboard{
	Paste: []string{"/usr/bin/pbpaste"},
	Copy:  []string{"/usr/bin/pbcopy"},
	Env:   []string{"LANG=en_US.UTF-8", "LC_CTYPE=en_US.UTF-8"},
}

func (p Pasteboard) run(args []string, input []byte, output *limitedBuffer) error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, args[0], args[1:]...)
	cmd.Env = append(os.Environ(), p.Env...)
	cmd.Stdin = bytes.NewReader(input)
	var errs limitedBuffer
	errs.limit = 512
	cmd.Stderr = &errs
	if output != nil {
		cmd.Stdout = output
	}
	err := cmd.Run()
	switch {
	case output != nil && output.over:
		return errClipboardTooBig
	case ctx.Err() != nil:
		return errClipboardTimeout
	case err != nil:
		why := strings.TrimSpace(errs.String())
		if why == "" {
			why = err.Error()
		}
		return fmt.Errorf("%s: %s", args[0], why)
	}
	return nil
}

// SetText makes text the clipboard's.
func (p Pasteboard) SetText(text string) error {
	if len(text) > maxClipboard {
		return errClipboardTooBig
	}
	return p.run(p.Copy, []byte(strings.ReplaceAll(text, "\r\n", "\n")), nil)
}

// Text is the clipboard's text, "" when it holds none.
func (p Pasteboard) Text() (string, error) {
	out := limitedBuffer{limit: maxClipboard}
	if err := p.run(p.Paste, nil, &out); err != nil {
		return "", err
	}
	text := out.String()
	if !utf8.ValidString(text) {
		text = strings.ToValidUTF8(text, "�")
	}
	return text, nil
}

// limitedBuffer keeps up to limit bytes and notes whether more came. Its
// only writer is Write (a bytes.Buffer inside would bring ReadFrom, which
// io.Copy would use instead).
type limitedBuffer struct {
	buf   bytes.Buffer
	limit int
	over  bool
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	if room := b.limit - b.buf.Len(); len(p) > room {
		b.over = true
		b.buf.Write(p[:max(room, 0)])
		return len(p), nil
	}
	return b.buf.Write(p)
}

func (b *limitedBuffer) String() string { return b.buf.String() }

var _ Clipboard = Pasteboard{}
