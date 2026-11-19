// SPDX-License-Identifier: AGPL-3.0-or-later

package sdk

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func resetRandom(t *testing.T, seed string) {
	t.Helper()
	t.Setenv("HARNESS_SEED", seed)
	random.Lock()
	random.device = filepath.Join(t.TempDir(), "missing")
	random.native = nil
	random.Unlock()
}

func TestNativeRandomFollowsTheSeed(t *testing.T) {
	resetRandom(t, "42")
	first := []uint64{Random(), Random(), Random()}
	resetRandom(t, "42")
	second := []uint64{Random(), Random(), Random()}
	for index := range first {
		if first[index] != second[index] {
			t.Fatalf("draw %d differs: %d vs %d", index, first[index], second[index])
		}
	}
	resetRandom(t, "43")
	if Random() == first[0] {
		t.Fatal("a different seed repeated the first draw")
	}
	if Below("test", 0) != 0 || Below("test", 1) != 0 {
		t.Fatal("Below must stay inside its range")
	}
}

func TestNativeBelowNeverAsksTheHost(t *testing.T) {
	resetRandom(t, "7")
	saved := choice.exchange
	defer func() { choice.exchange = saved }()
	choice.exchange = func([]byte) ([]byte, error) {
		t.Fatal("a native draw asked the host")
		return nil, nil
	}
	for range 32 {
		if Below("test", 5) >= 5 {
			t.Fatal("Below must stay inside its range")
		}
	}
}

func answerFrame(request []byte, payload ...byte) []byte {
	frame := make([]byte, frameHeader, frameHeader+len(payload))
	copy(frame, request[:frameHeader])
	binary.LittleEndian.PutUint16(frame[4:], kindResponse)
	binary.LittleEndian.PutUint32(frame[16:], uint32(len(payload)))
	return append(frame, payload...)
}

func TestAChoiceQuestionCarriesItsSiteDefaultAndClock(t *testing.T) {
	request := choiceRequest(9, 0x0102, "register.op", 4, 3, 0x0a0b)
	want := []byte{
		0x48, 0x43, 0x50, 0x31, 1, 0, 6, 0, 3, 0, 0, 0, 9, 0, 0, 0, 33, 0, 0, 0, 0, 0, 0, 0,
		11, 0, 2, 1, 0, 0, 0, 0, 0, 0, 4, 0, 3, 0, 0x0b, 0x0a, 0, 0, 0, 0, 0, 0,
	}
	want = append(want, "register.op"...)
	if !bytes.Equal(request, want) {
		t.Fatalf("request %x, want %x", request, want)
	}
	if label, err := choiceAnswer(answerFrame(request, 0), 9, 4, 3); err != nil || label != 3 {
		t.Fatalf("nominal answer gave %d, %v", label, err)
	}
	if label, err := choiceAnswer(answerFrame(request, 1, 2, 0), 9, 4, 3); err != nil || label != 2 {
		t.Fatalf("label answer gave %d, %v", label, err)
	}
	for _, payload := range [][]byte{{1, 4, 0}, {1}, {2}, {}} {
		if _, err := choiceAnswer(answerFrame(request, payload...), 9, 4, 3); err == nil {
			t.Fatalf("payload %x was accepted", payload)
		}
	}
	if _, err := choiceAnswer(answerFrame(request, 0), 8, 4, 3); err == nil {
		t.Fatal("a response for another request was accepted")
	}
	failed := answerFrame(request, 0)
	binary.LittleEndian.PutUint16(failed[10:], 2)
	if _, err := choiceAnswer(failed, 9, 4, 3); err == nil {
		t.Fatal("a failed status was accepted")
	}
}

func TestAskSendsOneQuestionPerDrawAndTakesTheAnswer(t *testing.T) {
	saved := choice.exchange
	defer func() { choice.exchange = saved }()
	var questions [][]byte
	choice.exchange = func(request []byte) ([]byte, error) {
		questions = append(questions, request)
		if len(questions) == 2 {
			return answerFrame(request, 1, 0, 0), nil
		}
		return answerFrame(request, 0), nil
	}
	if got := ask("runner.pause", 21, 7); got != 7 {
		t.Fatalf("nominal ask returned %d", got)
	}
	if got := ask("runner.pause", 21, 7); got != 0 {
		t.Fatalf("label ask returned %d", got)
	}
	if len(questions) != 2 || binary.LittleEndian.Uint32(questions[1][12:]) != binary.LittleEndian.Uint32(questions[0][12:])+1 {
		t.Fatal("each question takes the next sequence number")
	}
	defer func() {
		if recover() == nil {
			t.Fatal("an unnamed site was accepted")
		}
	}()
	ask("", 2, 0)
}

type decoded struct {
	Assert struct {
		Hit         bool            `json:"hit"`
		MustHit     bool            `json:"must_hit"`
		AssertType  string          `json:"assert_type"`
		DisplayType string          `json:"display_type"`
		Message     string          `json:"message"`
		Condition   bool            `json:"condition"`
		ID          string          `json:"id"`
		Details     json.RawMessage `json:"details"`
		Location    struct {
			File      string `json:"file"`
			Function  string `json:"function"`
			BeginLine int    `json:"begin_line"`
		} `json:"location"`
	} `json:"antithesis_assert"`
}

func TestAssertionsWriteOneRecordPerLine(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", dir)
	Declare(KindAlways, "declared")
	Always(false, "always broken", map[string]any{"value": "a-1-2"})
	Sometimes(true, "sometimes seen", nil)
	Reachable("reached", nil)
	Unreachable("never", nil)

	file, err := os.Open(filepath.Join(dir, "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	var records []decoded
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		var record decoded
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			t.Fatalf("line %q: %v", scanner.Text(), err)
		}
		records = append(records, record)
	}
	if len(records) != 5 {
		t.Fatalf("got %d records", len(records))
	}
	type want struct {
		assertType      string
		hit, must, cond bool
	}
	expected := []want{
		{"always", false, true, false},
		{"always", true, true, false},
		{"sometimes", true, true, true},
		{"reachability", true, true, true},
		{"reachability", true, false, true},
	}
	for index, record := range records {
		got := want{record.Assert.AssertType, record.Assert.Hit, record.Assert.MustHit, record.Assert.Condition}
		if got != expected[index] {
			t.Errorf("record %d = %+v, want %+v", index, got, expected[index])
		}
		if record.Assert.ID != record.Assert.Message || record.Assert.ID == "" {
			t.Errorf("record %d id %q message %q", index, record.Assert.ID, record.Assert.Message)
		}
		if record.Assert.Location.Function == "" || record.Assert.Location.BeginLine == 0 {
			t.Errorf("record %d has no caller location", index)
		}
	}
	if string(records[1].Assert.Details) != `{"value":"a-1-2"}` {
		t.Errorf("details = %s", records[1].Assert.Details)
	}
	if string(records[2].Assert.Details) != "null" {
		t.Errorf("empty details = %s", records[2].Assert.Details)
	}
}

func TestCheckUndecidedWritesOneRecordOnlyForUndecidedKeys(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", dir)
	CheckUndecided(0)
	CheckUndecided(3)
	text, err := os.ReadFile(filepath.Join(dir, "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if string(text) != "{\"harmony_check\":{\"undecided_keys\":3}}\n" {
		t.Fatalf("records %q", text)
	}
}

func TestNoOutputDirectoryIsSilent(t *testing.T) {
	t.Setenv("ANTITHESIS_OUTPUT_DIR", "")
	Always(false, "nowhere", nil)
}
