// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"reflect"
	"testing"

	"harmony.local/harness/kit/history"
)

const drive = "drive-1.jsonl"

func (b *builder) appendValue(p int, value string, invoke int64, outcome history.Type, position any, complete int64) {
	b.op(drive, p, LogAppend, "", value, invoke, outcome, position, complete)
}

func (b *builder) readValues(p int, values []string, invoke, complete int64) {
	b.op(drive, p, LogRead, "", nil, invoke, history.OK, ReadOutput{Add: values, N: len(values)}, complete)
}

func TestLogCleanHistoryWithConcurrencyAndIndeterminateOps(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.appendValue(1, "b", 3, history.OK, 2, 4)
	b.appendValue(2, "c", 3, history.Info, nil, 0)
	b.appendValue(3, "d", 5, history.Fail, nil, 6)
	b.appendValue(3, "e", 7, history.OK, 3, 20)
	b.readValues(0, []string{"a", "b"}, 5, 6)
	b.readValues(1, []string{"a", "b", "c"}, 7, 8)
	b.readValues(2, []string{"a", "b", "c", "e"}, 9, 10)
	b.op(drive, 3, LogRead, "", nil, 11, history.Info, nil, 0)
	b.op(drive, 0, LogRead, "", nil, 11, history.Fail, nil, 12)
	report := Log{}.Check(b.build())
	expectKinds(t, report)
	if !report.Decided || !report.Facts[LogAppendAcked] {
		t.Fatalf("report %+v", report)
	}
}

func TestLogLostAcknowledgedAppend(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.appendValue(1, "b", 1, history.OK, 2, 3)
	b.readValues(2, []string{"a", "b"}, 4, 5)
	b.readValues(2, []string{"a"}, 6, 7)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogLost)
	details := report.Violations[0].Details
	if details["value"] != "b" || details["missing"] != 1 || details["position"] != uint64(2) {
		t.Fatalf("details %+v", details)
	}
}

func TestLogReadConcurrentWithAckIsNotLoss(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 5)
	b.readValues(1, nil, 2, 3)
	b.readValues(1, nil, 5, 6)
	b.appendValue(0, "b", 7, history.Info, nil, 0)
	b.readValues(1, []string{"a"}, 8, 9)
	expectKinds(t, Log{}.Check(b.build()))
}

func TestLogOrderDivergence(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readValues(2, []string{"a", "b"}, 2, 3)
	b.readValues(3, []string{"b", "a"}, 2, 4)
	expectKinds(t, Log{}.Check(b.build()), LogOrder)
}

func TestLogLaterReadInsertsBeforeObservedValue(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.appendValue(2, "c", 1, history.Info, nil, 0)
	b.readValues(3, []string{"a", "c"}, 2, 3)
	b.readValues(3, []string{"a", "b", "c"}, 4, 5)
	expectKinds(t, Log{}.Check(b.build()), LogOrder)

	concurrent := newBuilder(t)
	concurrent.appendValue(0, "a", 1, history.Info, nil, 0)
	concurrent.appendValue(1, "b", 1, history.Info, nil, 0)
	concurrent.appendValue(2, "c", 1, history.Info, nil, 0)
	concurrent.readValues(3, []string{"a", "c"}, 2, 5)
	concurrent.readValues(4, []string{"a", "b", "c"}, 4, 6)
	expectKinds(t, Log{}.Check(concurrent.build()))
}

func TestLogDuplicateAndPhantom(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.readValues(1, []string{"a", "a"}, 2, 3)
	expectKinds(t, Log{}.Check(b.build()), LogDuplicate)

	p := newBuilder(t)
	p.appendValue(0, "a", 1, history.Info, nil, 0)
	p.readValues(1, []string{"a", "z"}, 3, 4)
	report := Log{}.Check(p.build())
	expectKinds(t, report, LogPhantom)
	if report.Violations[0].Details["value"] != "z" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogFailedAppendInARead(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.appendValue(1, "b", 1, history.Fail, nil, 2)
	b.readValues(2, []string{"a", "b"}, 3, 4)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogFailedRead)
	if report.Violations[0].Details["value"] != "b" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogSharedPosition(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 4, 2)
	b.appendValue(1, "b", 3, history.OK, 4, 4)
	b.appendValue(2, "c", 3, history.OK, nil, 4)
	b.appendValue(3, "d", 3, history.Info, 4, 0)
	expectKinds(t, Log{}.Check(b.build()), LogPosition)
}

func TestLogIndeterminateOpsNeverViolate(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Fail, nil, 2)
	b.appendValue(2, "c", 1, "", nil, 0)
	b.readValues(3, []string{"a"}, 3, 4)
	b.readValues(3, nil, 5, 6)
	b.readValues(3, []string{"c"}, 7, 8)
	b.op(drive, 0, LogRead, "", nil, 9, history.Info, nil, 0)
	report := Log{}.Check(b.build())
	expectKinds(t, report)
	if report.Facts[LogAppendAcked] {
		t.Fatal("no append was acknowledged")
	}
}

func TestLogNoReadIsUndecided(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.op(drive, 1, LogRead, "", nil, 3, history.Fail, nil, 4)
	report := Log{}.Check(b.build())
	if report.Decided || len(report.Violations) != 0 {
		t.Fatalf("report %+v", report)
	}
}

func TestLogRecorderWritesDeltasThatResolve(t *testing.T) {
	dir := t.TempDir()
	tokens := uint64(0)
	token := func() uint64 { tokens++; return tokens }
	journal, err := history.Create(dir, "drive", token)
	if err != nil {
		t.Fatal(err)
	}
	recorder := Log{}.Recorder(nil)
	reads := [][]string{{"a", "b"}, {"a", "b", "c"}, {"a"}, nil, {"a", "x", "y"}}
	for _, values := range reads {
		index, err := journal.Invoke(0, LogRead, "", nil)
		if err != nil {
			t.Fatal(err)
		}
		if err := recorder.Record(journal, 0, Op{F: LogRead}, index, values); err != nil {
			t.Fatal(err)
		}
	}
	journal.Close()
	prior, err := history.Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	check, err := history.Create(dir, "check", token)
	if err != nil {
		t.Fatal(err)
	}
	seeded := Log{}.Recorder(prior)
	index, _ := check.Invoke(0, LogRead, "", nil)
	if err := seeded.Record(check, 0, Op{F: LogRead}, index, []string{"a", "x", "y", "z"}); err != nil {
		t.Fatal(err)
	}
	check.Close()
	h, err := history.Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	last, ok := h.Lookup(history.Ref{Journal: check.Name(), Index: index})
	if !ok || string(last.Output) != `{"b":{"j":"`+journal.Name()+`","i":4},"keep":3,"add":["z"],"n":4}` {
		t.Fatalf("check read recorded as %s", last.Output)
	}
	state := resolveLog(h)
	want := append(reads, []string{"a", "x", "y", "z"})
	if state.unresolved != 0 || len(state.reads) != len(want) {
		t.Fatalf("unresolved=%d reads=%d", state.unresolved, len(state.reads))
	}
	for index, read := range state.reads {
		got := read.values()
		if len(got) == 0 && len(want[index]) == 0 {
			continue
		}
		if !reflect.DeepEqual(got, want[index]) {
			t.Fatalf("read %d resolved to %v, want %v", index, got, want[index])
		}
	}
	if len(state.arenas) != 2 {
		t.Fatalf("arenas = %d", len(state.arenas))
	}
}

func TestLogUnresolvableReadIsIgnored(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.op(drive, 1, LogRead, "", nil, 3, history.OK,
		ReadOutput{Base: &history.Ref{Journal: "gone.jsonl", Index: 9}, Keep: 1, Add: nil, N: 1}, 4)
	report := Log{}.Check(b.build())
	if report.Decided || len(report.Violations) != 0 {
		t.Fatalf("report %+v", report)
	}
}

func TestLogGeneratorMakesBothOps(t *testing.T) {
	generator := Log{}.Generator(0)
	seen := map[string]int{}
	next := uint64(0)
	for range 64 {
		op := generator.Next(func(n uint64) uint64 { next++; return next % n }, func() string { return "v" })
		seen[op.F]++
	}
	if seen[LogRead] == 0 || seen[LogAppend] == 0 {
		t.Fatalf("ops %v", seen)
	}
}
