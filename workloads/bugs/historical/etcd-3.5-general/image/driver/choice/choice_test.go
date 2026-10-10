// SPDX-License-Identifier: AGPL-3.0-or-later

package choice

import (
	"encoding/binary"
	"testing"
)

func TestTheRequestIsAnSDKServiceFrameForNamespaceEleven(t *testing.T) {
	frame := Request(7)
	if len(frame) != 34 {
		t.Fatalf("length %d", len(frame))
	}
	want := map[int]uint32{0: frameMagic, 12: 7, 16: 10}
	for at, value := range want {
		if got := binary.LittleEndian.Uint32(frame[at:]); got != value {
			t.Fatalf("offset %d: %#x", at, got)
		}
	}
	if binary.LittleEndian.Uint16(frame[6:]) != serviceSDK || binary.LittleEndian.Uint16(frame[8:]) != serviceRequest {
		t.Fatal("service or opcode")
	}
	if binary.LittleEndian.Uint16(frame[24:]) != choiceNamespace {
		t.Fatal("namespace")
	}
}

func response(status uint16, body []byte) []byte {
	frame := make([]byte, headerLen+len(body))
	binary.LittleEndian.PutUint32(frame[0:], frameMagic)
	binary.LittleEndian.PutUint16(frame[4:], 2)
	binary.LittleEndian.PutUint16(frame[10:], status)
	binary.LittleEndian.PutUint32(frame[16:], uint32(len(body)))
	copy(frame[headerLen:], body)
	return frame
}

func TestParseAcceptsOnlyAnAnsweredChoice(t *testing.T) {
	body := append([]byte{1}, binary.LittleEndian.AppendUint64(nil, 42)...)
	if choice, ok := Parse(response(0, body)); !ok || choice != 42 {
		t.Fatal(choice, ok)
	}
	for _, bad := range [][]byte{response(5, body), response(0, []byte{0}), response(0, body[:5])} {
		if _, ok := Parse(bad); ok {
			t.Fatal("accepted", bad)
		}
	}
}

func draws(s *Stream) [4]uint64 {
	var out [4]uint64
	for i := range out {
		out[i] = s.Next()
	}
	return out
}

func TestARestoredStreamDivergesUnderANewChoiceAndRepeatsUnderTheSameOne(t *testing.T) {
	restored := func(choice uint64) [4]uint64 {
		s := NewSeeded(99)
		s.Fold(1)
		_ = s.Next()
		s.Fold(choice)
		return draws(s)
	}
	if restored(2) != restored(2) {
		t.Fatal("the same choice must replay the same operations")
	}
	if restored(2) == restored(3) {
		t.Fatal("a new choice must change the continuation")
	}
	s := NewSeeded(99)
	s.Fold(5)
	before := s.state
	s.Fold(5)
	if s.state != before {
		t.Fatal("folding an unchanged choice changes nothing")
	}
}

func TestASiteFollowsItsByteOfTheChoice(t *testing.T) {
	unfolded := NewSeeded(1)
	if unfolded.Bias(0, 4) != -1 {
		t.Fatal("a stream without a choice biased a site")
	}
	s := NewSeeded(1)
	s.Fold(uint64(0xc0|5)<<(8*3) | uint64(0x00|2)<<(8*1))
	follows := 0
	for i := 0; i < 8000; i++ {
		if s.Bias(1, 4) != -1 {
			t.Fatal("a byte with no strength biased its site")
		}
		switch s.Bias(3, 4) {
		case 1:
			follows++
		case -1:
		default:
			t.Fatal("a biased site preferred an option other than its byte's")
		}
	}
	if follows < 6600 || follows > 7400 {
		t.Fatalf("a full-strength site followed its choice %d times in 8000", follows)
	}
	if s.Bias(11, 4) == -1 && s.Bias(11, 4) == -1 && s.Bias(11, 4) == -1 && s.Bias(11, 4) == -1 {
		t.Fatal("site 11 did not share site 3's byte")
	}
}
