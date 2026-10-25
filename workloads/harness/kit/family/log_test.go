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
	b.readValues(3, nil, 3, 4)
	b.readValues(3, []string{"a"}, 5, 6)
	b.readValues(3, []string{"a", "c"}, 7, 8)
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
		index, err := journal.Invoke(0, LogRead, "", "", nil)
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
	index, _ := check.Invoke(0, LogRead, "", "", nil)
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

func (b *builder) readOn(p int, node string, values []string, invoke, complete int64) {
	b.opOn(drive, p, LogRead, "", node, nil, invoke, history.OK, ReadOutput{Add: values, N: len(values)}, complete)
}

func TestLogNodeReadThatSkipsAcknowledgedAppends(t *testing.T) {
	b := newBuilder(t)
	for index, value := range []string{"a", "b", "c", "d", "e"} {
		b.appendValue(0, value, int64(1+2*index), history.OK, index+1, int64(2+2*index))
	}
	b.readValues(1, []string{"a", "b", "c", "d", "e"}, 20, 21)
	b.readOn(2, "n1", []string{"a", "b", "c", "d", "e"}, 20, 22)
	b.readOn(3, "n2", []string{"a", "b", "e"}, 20, 23)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeLost)
	details := report.Violations[0].Details
	if details["node"] != "n2" || details["value"] != "c" || details["missing"] != 2 ||
		!reflect.DeepEqual(details["missing_values"], []string{"c", "d"}) || details["node_occurrences"] != 1 {
		t.Fatalf("details %+v", details)
	}
	if len(report.Undecided) != 0 {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestLogNodeReadIsNotGlobalLoss(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.readOn(1, "n3", nil, 3, 4)
	b.readValues(2, []string{"a"}, 3, 4)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeLost)
	if report.Violations[0].Details["node"] != "n3" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogNodeReadOrderDivergence(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readOn(2, "n3", []string{"a", "b"}, 2, 3)
	b.readOn(3, "n3", []string{"b", "a"}, 2, 4)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeOrder)
	if len(report.Violations) != 1 || report.Violations[0].Details["node"] != "n3" {
		t.Fatalf("violations %+v", report.Violations)
	}
}

func TestLogReadsOnDifferentNodesAreNotCompared(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readValues(2, []string{"a", "b"}, 2, 3)
	b.readOn(3, "n1", []string{"a", "b"}, 2, 3)
	b.readOn(4, "n2", []string{"b", "a"}, 2, 4)
	expectKinds(t, Log{}.Check(b.build()))
}

func TestLogNodeReadInsertsBeforeObservedValue(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.appendValue(2, "c", 1, history.Info, nil, 0)
	b.readOn(3, "n1", []string{"a", "c"}, 2, 3)
	b.readOn(3, "n1", []string{"a", "b", "c"}, 4, 5)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeOrder)
	if report.Violations[0].Details["node"] != "n1" || report.Violations[0].Details["inserted_value"] != "b" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogGlobalOrderViolationStaysGlobal(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readValues(2, []string{"a", "b"}, 2, 3)
	b.readValues(3, []string{"b", "a"}, 2, 4)
	b.readOn(4, "n1", []string{"a"}, 2, 4)
	expectKinds(t, Log{}.Check(b.build()), LogOrder)
}

func TestLogNodeEvidenceIsCappedPerNode(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	for index := range 4 {
		b.readOn(1, "n1", nil, int64(3+2*index), int64(4+2*index))
	}
	b.readOn(2, "n2", nil, 3, 4)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeLost)
	perNode := map[any]int{}
	for _, violation := range report.Violations {
		node := violation.Details["node"]
		perNode[node]++
		want := map[any]int{"n1": 4, "n2": 1}[node]
		if violation.Details["occurrences"] != 5 || violation.Details["node_occurrences"] != want {
			t.Fatalf("details %+v", violation.Details)
		}
	}
	if perNode["n1"] != 3 || perNode["n2"] != 1 {
		t.Fatalf("evidence per node %v", perNode)
	}
}

func TestLogNodePhantomNamesTheNode(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.readOn(1, "n2", []string{"a", "z"}, 3, 4)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogPhantom)
	if report.Violations[0].Details["node"] != "n2" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogUndecidedKindsFollowReadKinds(t *testing.T) {
	unpinned := newBuilder(t)
	unpinned.appendValue(0, "a", 1, history.OK, 1, 2)
	unpinned.readValues(1, []string{"a"}, 3, 4)
	report := Log{}.Check(unpinned.build())
	if !report.Decided || !report.Undecided[LogNodeLost] || !report.Undecided[LogNodeOrder] || report.Undecided[LogLost] {
		t.Fatalf("report %+v", report)
	}
	pinned := newBuilder(t)
	pinned.appendValue(0, "a", 1, history.OK, 1, 2)
	pinned.readOn(1, "n1", []string{"a"}, 3, 4)
	report = Log{}.Check(pinned.build())
	if !report.Decided || !report.Undecided[LogLost] || !report.Undecided[LogOrder] || report.Undecided[LogNodeLost] {
		t.Fatalf("report %+v", report)
	}
}

func TestLogRecorderChainsEachNodeToItsOwnReads(t *testing.T) {
	dir := t.TempDir()
	tokens := uint64(0)
	token := func() uint64 { tokens++; return tokens }
	journal, err := history.Create(dir, "check", token)
	if err != nil {
		t.Fatal(err)
	}
	recorder := Log{}.Recorder(nil)
	reads := []struct {
		node   string
		values []string
	}{
		{"", []string{"a", "b", "c"}},
		{"n2", []string{"a", "c"}},
		{"", []string{"a", "b", "c", "d"}},
		{"n2", []string{"a", "c", "d"}},
		{"n1", []string{"a", "b", "c", "d"}},
	}
	indexes := make([]int64, len(reads))
	for at, read := range reads {
		op := Op{F: LogRead, Node: read.node}
		index, err := journal.Invoke(at, op.F, op.K, op.Node, nil)
		if err != nil {
			t.Fatal(err)
		}
		indexes[at] = index
		if err := recorder.Record(journal, at, op, index, read.values); err != nil {
			t.Fatal(err)
		}
	}
	journal.Close()
	h, err := history.Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	second, _ := h.Lookup(history.Ref{Journal: journal.Name(), Index: indexes[3]})
	if string(second.Output) != `{"b":{"j":"`+journal.Name()+`","i":1},"keep":2,"add":["d"],"n":3}` {
		t.Fatalf("second n2 read recorded as %s", second.Output)
	}
	first, _ := h.Lookup(history.Ref{Journal: journal.Name(), Index: indexes[4]})
	if string(first.Output) != `{"b":{"j":"`+journal.Name()+`","i":2},"keep":4,"add":[],"n":4}` {
		t.Fatalf("first n1 read recorded as %s", first.Output)
	}
	state := resolveLog(h)
	if state.unresolved != 0 || len(state.reads) != len(reads) {
		t.Fatalf("unresolved=%d reads=%d", state.unresolved, len(state.reads))
	}
	for at, read := range state.reads {
		if read.op.Node != reads[at].node || !reflect.DeepEqual(read.values(), reads[at].values) {
			t.Fatalf("read %d on %q resolved to %v", at, read.op.Node, read.values())
		}
	}
	seeded := Log{}.Recorder(h).(*logRecorder)
	if seeded.chains["n2"].base.Index != indexes[3] || seeded.chains[""].base.Index != indexes[2] {
		t.Fatalf("seeded chains %+v", seeded.chains)
	}
}

func TestLogReadLosesValueAnEarlierReadReturned(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.OK, 1, 2)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.appendValue(2, "c", 1, history.Info, nil, 0)
	b.readValues(3, []string{"a", "b"}, 3, 4)
	b.readValues(3, []string{"a", "c"}, 5, 6)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogReadLost)
	details := report.Violations[0].Details
	if details["value"] != "b" || details["position"] != 1 || details["missing"] != 1 {
		t.Fatalf("details %+v", details)
	}
}

func TestLogReadConcurrentWithEarlierReadIsNotLoss(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readValues(2, []string{"a", "b"}, 2, 5)
	b.readValues(3, []string{"a"}, 4, 6)
	expectKinds(t, Log{}.Check(b.build()))
}

func TestLogNodeReadLosesValueAnEarlierReadReturned(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readValues(2, []string{"a", "b"}, 2, 3)
	b.readOn(3, "n2", []string{"a"}, 4, 5)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogNodeReadLost)
	if report.Violations[0].Details["node"] != "n2" || report.Violations[0].Details["value"] != "b" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestLogNodeReadIsNotASourceOfReturnedValues(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.readOn(2, "n1", []string{"a", "b"}, 2, 3)
	b.readValues(3, []string{"a"}, 4, 5)
	expectKinds(t, Log{}.Check(b.build()))
}

func TestLogReadWithFailedValueIsNotASource(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "f", 1, history.Fail, nil, 2)
	b.readValues(1, []string{"f"}, 3, 4)
	b.readValues(1, nil, 5, 6)
	expectKinds(t, Log{}.Check(b.build()), LogFailedRead)
}

func TestLogWidestEarlierReadIsTheSource(t *testing.T) {
	b := newBuilder(t)
	b.appendValue(0, "a", 1, history.Info, nil, 0)
	b.appendValue(1, "b", 1, history.Info, nil, 0)
	b.appendValue(2, "c", 1, history.Info, nil, 0)
	b.readValues(3, []string{"a"}, 1, 2)
	b.readValues(3, []string{"a", "b", "c"}, 3, 4)
	b.readValues(3, []string{"a"}, 5, 6)
	report := Log{}.Check(b.build())
	expectKinds(t, report, LogReadLost)
	details := report.Violations[0].Details
	if details["value"] != "b" || details["missing"] != 2 {
		t.Fatalf("details %+v", details)
	}
}
