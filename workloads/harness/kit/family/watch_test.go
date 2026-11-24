// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"testing"

	"harmony.local/harness/kit/history"
)

func ev(revision uint64, key, value string) Event {
	return Event{Revision: revision, Key: key, Value: &value}
}

func (b *builder) put(p int, key, value string, invoke int64, revision uint64) {
	b.op(drive, p, RegisterWrite, key, value, invoke, history.OK, revision, invoke+1)
}

func (b *builder) watch(p int, from uint64, invoke int64, events ...Event) {
	b.op(drive, p, RegisterWatch, "", WatchQuery{From: from}, invoke, history.OK, events, invoke+1)
}

func (b *builder) history(p int, node string, invoke int64, events ...Event) {
	b.opOn(check, p, RegisterHistory, "", node, nil, invoke, history.OK, events, invoke+1)
}

func watchHistory(t *testing.T) *builder {
	b := newBuilder(t)
	b.put(0, "k0", "a", 1, 2)
	b.put(1, "k1", "b", 3, 3)
	b.put(0, "k0", "c", 5, 4)
	b.put(1, "k1", "d", 7, 5)
	return b
}

var reference = []Event{ev(2, "k0", "a"), ev(3, "k1", "b"), ev(4, "k0", "c"), ev(5, "k1", "d")}

func TestWatchClean(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 3, 2, ev(3, "k1", "b"), ev(4, "k0", "c"), ev(5, "k1", "d"))
	b.watch(3, 9, 8)
	b.history(10, "n1", 20, reference...)
	b.history(11, "n2", 20, reference[:3]...)
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report)
	for _, kind := range watchKinds() {
		if report.Undecided[kind] {
			t.Fatalf("undecided %v", report.Undecided)
		}
	}
}

func TestWatchEventBelowStart(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 4, 2, ev(3, "k1", "b"), ev(4, "k0", "c"), ev(5, "k1", "d"))
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchFloor)
}

func TestWatchDuplicateEvent(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 2, 2, ev(2, "k0", "a"), ev(3, "k1", "b"), ev(3, "k1", "b"), ev(4, "k0", "c"))
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchOrder)
}

func TestWatchGap(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 2, 2, ev(2, "k0", "a"), ev(4, "k0", "c"))
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchComplete)
}

func TestWatchGapAgainstAcknowledgedWrites(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 2, 2, ev(2, "k0", "a"), ev(4, "k0", "c"))
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchComplete)
}

func TestWatchWrongValue(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 3, 2, ev(3, "k1", "x"))
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchMatches, RegisterWatchComplete)
}

func TestWatchValueAtWrongRevisionWithoutReference(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 4, 2, ev(4, "k1", "b"))
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchMatches, RegisterWatchComplete)
}

func TestMemberHistoryDisagrees(t *testing.T) {
	b := watchHistory(t)
	b.history(10, "n1", 20, reference...)
	b.history(11, "n2", 20, ev(2, "k0", "a"), ev(3, "k1", "b"), ev(4, "k0", "z"))
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report, RegisterHistoryAgrees)
	if report.Violations[0].Details["node"] != "n2" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestWatchOffAddsNoKinds(t *testing.T) {
	for _, kind := range (Register{}).Kinds() {
		for _, watch := range watchKinds() {
			if kind == watch {
				t.Fatalf("kind %q without watches", kind)
			}
		}
	}
	if _, on := (Register{}).Feed(); on {
		t.Fatal("history read without watches")
	}
}

func TestRegisterGeneratorDrawsWatchStarts(t *testing.T) {
	generator := Register{Watch: 2}.Generator(0)
	generator.Observe(Op{F: RegisterWrite, K: "k0", V: "x"}, history.OK, uint64(40))
	for from, want := range map[uint64]uint64{0: 35, 1: 41, 2: 57} {
		op := generator.Next(func(site string, n uint64) uint64 {
			switch site {
			case "register.watch":
				return 0
			case "register.watch.from":
				return from
			}
			return 5 + 5*from
		}, func() string { return "v" })
		if op.F != RegisterWatch || op.V.(WatchQuery).From != want {
			t.Fatalf("from choice %d: op %+v, want start %d", from, op, want)
		}
	}
}

func from(start uint64, events ...Event) []Event {
	for index := range events {
		events[index].Start = start
	}
	return events
}

func TestWatchResumedSegmentBelowItsOwnStart(t *testing.T) {
	b := watchHistory(t)
	events := append(from(2, ev(2, "k0", "a"), ev(3, "k1", "b")), from(4, ev(3, "k1", "b"), ev(4, "k0", "c"))...)
	b.watch(2, 2, 2, events...)
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchFloor, RegisterWatchOrder)
}

func TestWatchEventsAtOneRevisionInAnyKeyOrder(t *testing.T) {
	b := newBuilder(t)
	b.watch(2, 7, 2, ev(7, "k1", "x"), ev(7, "k0", "y"))
	at7 := []Event{ev(7, "k0", "y"), ev(7, "k1", "x")}
	b.history(10, "n1", 20, at7...)
	b.history(11, "n2", 20, ev(7, "k1", "x"), ev(7, "k0", "y"))
	expectKinds(t, Register{Watch: 4}.Check(b.build()))
}

func TestWatchRepeatedKeyAtOneRevision(t *testing.T) {
	b := newBuilder(t)
	b.watch(2, 7, 2, ev(7, "k0", "y"), ev(7, "k1", "x"), ev(7, "k0", "y"))
	b.history(10, "n1", 20, ev(7, "k0", "y"), ev(7, "k1", "x"))
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchOrder)
}

func TestWatchDeleteBeyondTheReferenceIsNotAMismatch(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 5, 2, ev(5, "k1", "d"), Event{Revision: 6, Key: "k1"})
	b.history(10, "n1", 20, reference...)
	expectKinds(t, Register{Watch: 4}.Check(b.build()))
}

func TestOneMemberDecidesNoHistoryAgreement(t *testing.T) {
	b := newBuilder(t)
	b.history(10, "n1", 20, reference...)
	b.history(11, "n1", 30, reference[:2]...)
	b.history(12, "n1", 40, ev(2, "k0", "a"), ev(3, "k1", "z"))
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report)
	if !report.Undecided[RegisterHistoryAgrees] {
		t.Fatalf("agreement decided by one member: %v", report.Undecided)
	}
}

func TestAnIncompleteReferenceHidesNoAcknowledgedWrite(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 2, 2, ev(2, "k0", "a"), ev(4, "k0", "c"))
	b.history(10, "n1", 20, ev(2, "k0", "a"), ev(4, "k0", "c"))
	expectKinds(t, Register{Watch: 4}.Check(b.build()), RegisterWatchComplete, RegisterHistoryAgrees)
}

func TestAWatchHoldingAnAcknowledgedWriteIsNotBlamedForTheReference(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 2, 2, ev(2, "k0", "a"), ev(3, "k1", "b"), ev(4, "k0", "c"))
	b.history(10, "n1", 20, ev(2, "k0", "a"), ev(4, "k0", "c"), ev(5, "k1", "d"))
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report, RegisterHistoryAgrees)
	if report.Violations[0].Details["node"] != "n1" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestAnIdleWatchDecidesNoWatchProperty(t *testing.T) {
	b := watchHistory(t)
	b.watch(2, 9, 8)
	report := Register{Watch: 4}.Check(b.build())
	for _, kind := range []string{RegisterWatchFloor, RegisterWatchOrder, RegisterWatchComplete, RegisterWatchMatches} {
		if !report.Undecided[kind] {
			t.Fatalf("%q decided by an idle watch: %v", kind, report.Undecided)
		}
	}
}

func TestAnEmptyHistoryDecidesNoAgreement(t *testing.T) {
	b := watchHistory(t)
	b.history(10, "n1", 20, reference...)
	b.history(11, "n2", 20)
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report)
	if !report.Undecided[RegisterHistoryAgrees] {
		t.Fatalf("agreement decided by an empty history: %v", report.Undecided)
	}
}

func TestAShorterHistoryIsComparedWithEveryOtherMember(t *testing.T) {
	b := newBuilder(t)
	b.history(10, "n1", 20, ev(2, "k0", "a"), ev(3, "k1", "b"), ev(5, "k0", "x"), ev(6, "k1", "y"))
	b.history(11, "n1", 30, ev(2, "k0", "a"), ev(3, "k1", "z"))
	b.history(12, "n2", 40, ev(2, "k0", "a"), ev(3, "k1", "b"), ev(5, "k0", "x"), ev(6, "k1", "y"))
	report := Register{Watch: 4}.Check(b.build())
	expectKinds(t, report, RegisterHistoryAgrees)
}
