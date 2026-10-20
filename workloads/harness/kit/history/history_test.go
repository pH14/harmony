// SPDX-License-Identifier: AGPL-3.0-or-later

package history

import (
	"os"
	"path/filepath"
	"testing"
)

func counter() func() uint64 {
	next := uint64(0)
	return func() uint64 {
		next++
		return next
	}
}

func TestJournalRoundTrip(t *testing.T) {
	dir := t.TempDir()
	token := counter()
	first, err := Create(dir, "drive", token)
	if err != nil {
		t.Fatal(err)
	}
	write, _ := first.Invoke(0, "append", "", "", "v-0-1")
	read, _ := first.Invoke(1, "read", "", "", nil)
	lost, _ := first.Invoke(2, "append", "", "", "v-2-1")
	failed, _ := first.Invoke(3, "append", "", "", "v-3-1")
	if err := first.Complete(0, OK, "append", "", write, 7); err != nil {
		t.Fatal(err)
	}
	if err := first.Complete(1, Info, "read", "", read, map[string]string{"error": "timeout"}); err != nil {
		t.Fatal(err)
	}
	if err := first.Complete(3, Fail, "append", "", failed, nil); err != nil {
		t.Fatal(err)
	}
	first.Close()

	second, err := Create(dir, "check", counter())
	if err != nil {
		t.Fatal(err)
	}
	if second.Name() == first.Name() {
		t.Fatal("journal names must differ")
	}
	checkRead, _ := second.Invoke(0, "read", "", "n2", nil)
	second.Close()
	appendTorn(t, filepath.Join(dir, second.Name()), `{"p":0,"t":"ok","f":"read","k":"","v":[],"ts":9`)

	h, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if h.Files != 2 || len(h.Ops) != 5 || h.Malformed != 0 {
		t.Fatalf("files=%d ops=%d malformed=%d", h.Files, len(h.Ops), h.Malformed)
	}
	types := map[Ref]Type{}
	for _, op := range h.Ops {
		types[Ref{op.Journal, op.Index}] = op.Type
		if pinned := op.Journal == second.Name(); (op.Node == "n2") != pinned {
			t.Errorf("op %s/%d has node %q", op.Journal, op.Index, op.Node)
		}
		if op.Type == Info && op.Complete != Forever {
			t.Errorf("indeterminate op %d completes at %d", op.Index, op.Complete)
		}
		if op.Definite() && op.Complete < op.Invoke {
			t.Errorf("op %d completes before it starts", op.Index)
		}
	}
	want := map[Ref]Type{
		{first.Name(), write}:      OK,
		{first.Name(), read}:       Info,
		{first.Name(), lost}:       Info,
		{first.Name(), failed}:     Fail,
		{second.Name(), checkRead}: Info,
	}
	for ref, expected := range want {
		if types[ref] != expected {
			t.Errorf("%v: %s, want %s", ref, types[ref], expected)
		}
	}
	op, ok := h.Lookup(Ref{first.Name(), write})
	if !ok || string(op.Output) != "7" || string(op.Input) != `"v-0-1"` {
		t.Fatalf("lookup = %+v", op)
	}
	for index := 1; index < len(h.Ops); index++ {
		if h.Ops[index-1].Invoke > h.Ops[index].Invoke {
			t.Fatal("ops are not ordered by invocation")
		}
	}
	if h.Ops[0].Process == h.Ops[4].Process && h.Ops[0].Journal != h.Ops[4].Journal {
		t.Fatal("processes from different journals share an id")
	}
}

func appendTorn(t *testing.T, path, text string) {
	t.Helper()
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	if _, err := file.WriteString(text); err != nil {
		t.Fatal(err)
	}
}

func TestCreateSkipsTakenNames(t *testing.T) {
	dir := t.TempDir()
	same := func() uint64 { return 5 }
	first, err := Create(dir, "drive", same)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	tokens := []uint64{5, 5, 6}
	second, err := Create(dir, "drive", func() uint64 {
		next := tokens[0]
		tokens = tokens[1:]
		return next
	})
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	if first.Name() == second.Name() {
		t.Fatal("a second journal reused a name")
	}
}

func TestMissingDirectoryIsEmpty(t *testing.T) {
	h, err := Load(filepath.Join(t.TempDir(), "absent"))
	if err != nil || len(h.Ops) != 0 {
		t.Fatalf("h=%+v err=%v", h, err)
	}
}

func TestOrphanCompletionsAreMalformed(t *testing.T) {
	h := Build([]File{{Name: "a.jsonl", Data: []byte(
		`{"p":0,"t":"ok","f":"read","k":"","v":null,"ts":1,"i":4}` + "\n" +
			`not json` + "\n" +
			`{"p":0,"t":"invoke","f":"read","k":"","v":null,"ts":2,"i":5}` + "\n" +
			`{"p":0,"t":"ok","f":"read","k":"","v":[],"ts":3,"i":5}` + "\n")}})
	if h.Malformed != 2 || len(h.Ops) != 1 || h.Ops[0].Type != OK || h.Ops[0].Complete != 3 {
		t.Fatalf("%+v", h)
	}
}
