// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"fmt"
	"slices"
	"testing"

	"harmony.local/harness/kit/history"
)

func (b *builder) write(p int, key, value string, invoke int64, outcome history.Type, complete int64) {
	b.op(drive, p, RegisterWrite, key, value, invoke, outcome, nil, complete)
}

func (b *builder) cas(p int, key, old, value string, invoke int64, outcome history.Type, complete int64) {
	b.op(drive, p, RegisterCAS, key, []string{old, value}, invoke, outcome, nil, complete)
}

func (b *builder) read(p int, key string, value any, invoke int64, complete int64) {
	b.op(drive, p, RegisterReadF, key, nil, invoke, history.OK, value, complete)
}

func TestRegisterClean(t *testing.T) {
	b := newBuilder(t)
	b.read(0, "k0", nil, 1, 2)
	b.write(0, "k0", "a", 3, history.OK, 4)
	b.read(1, "k0", "a", 5, 6)
	b.cas(2, "k0", "a", "b", 7, history.OK, 8)
	b.read(1, "k0", "b", 9, 10)
	b.write(3, "k1", "c", 9, history.OK, 12)
	b.read(0, "k1", "c", 10, 11)
	b.read(1, "k1", "c", 13, 14)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if !report.Decided || !report.Facts[RegisterWriteAcked] {
		t.Fatalf("report %+v", report)
	}
}

func TestRegisterStaleRead(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 4)
	b.read(1, "k0", "a", 5, 6)
	report := Register{}.Check(b.build())
	expectKinds(t, report, RegisterStale, RegisterLinearizable)
}

func TestRegisterMissingAfterAcknowledgedWrite(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.read(1, "k0", nil, 3, 4)
	expectKinds(t, Register{}.Check(b.build()), RegisterStale, RegisterLinearizable)
}

func TestRegisterNonLinearizableWithoutStaleness(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 10)
	b.read(1, "k0", "a", 2, 3)
	b.read(1, "k0", nil, 4, 5)
	expectKinds(t, Register{}.Check(b.build()), RegisterLinearizable)
}

func TestRegisterIndeterminateOpsNeverViolate(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.Info, 0)
	b.read(1, "k0", nil, 2, 3)
	b.read(1, "k0", "a", 4, 5)
	b.write(2, "k0", "b", 6, history.Fail, 7)
	b.read(1, "k0", "a", 8, 9)
	b.cas(3, "k0", "a", "c", 10, history.Info, 0)
	b.read(1, "k0", "a", 11, 12)
	b.cas(3, "k0", "a", "d", 13, history.Fail, 14)
	b.read(1, "k0", "a", 15, 16)
	b.write(2, "k0", "e", 17, "", 0)
	b.read(1, "k0", "a", 18, 19)
	b.op(drive, 1, RegisterReadF, "k0", nil, 20, history.Info, nil, 0)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
}

func TestRegisterUnseenIndeterminateWritesKeepTheCheckDecided(t *testing.T) {
	b := newBuilder(t)
	for p := 0; p < 24; p++ {
		b.write(10+p, "k0", fmt.Sprintf("lost%d", p), int64(1+p), history.Info, 0)
	}
	b.write(0, "k0", "a", 30, history.OK, 40)
	b.read(1, "k0", "a", 31, 32)
	b.read(1, "k0", nil, 33, 34)
	report := Register{Steps: 1000}.Check(b.build())
	expectKinds(t, report, RegisterLinearizable)
	if report.Undecided[RegisterLinearizable] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterIndeterminateWriteSeenByCASIsKept(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.Info, 0)
	b.cas(1, "k0", "a", "b", 3, history.OK, 4)
	b.read(2, "k0", "b", 5, 6)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if report.Undecided[RegisterLinearizable] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterStepBudgetLeavesLinearizabilityUndecided(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 10)
	b.read(1, "k0", "a", 2, 3)
	b.read(1, "k0", nil, 4, 5)
	report := Register{Steps: 1}.Check(b.build())
	expectKinds(t, report)
	if !report.Undecided[RegisterLinearizable] {
		t.Fatalf("undecided %v", report.Undecided)
	}
	if report.UndecidedKeys != 1 {
		t.Fatalf("undecided keys %d", report.UndecidedKeys)
	}
}

func TestRegisterCountsEachUndecidedKey(t *testing.T) {
	b := newBuilder(t)
	for index, key := range []string{"k0", "k1", "k2"} {
		at := int64(10 * index)
		b.write(0, key, "a"+key, at+1, history.OK, at+2)
		b.read(1, key, "a"+key, at+3, at+4)
	}
	b.write(0, "k3", "a", 31, history.OK, 32)
	report := Register{Steps: 1}.Check(b.build())
	expectKinds(t, report)
	if report.UndecidedKeys != 3 {
		t.Fatalf("undecided keys %d", report.UndecidedKeys)
	}
	if clean := (Register{}).Check(b.build()); clean.UndecidedKeys != 0 || clean.Undecided[RegisterLinearizable] {
		t.Fatalf("report %+v", clean)
	}
}

func (b *builder) readKeyOn(p int, node, key string, value any, invoke int64, complete int64) {
	b.opOn(drive, p, RegisterReadF, key, node, nil, invoke, history.OK, value, complete)
}

func TestRegisterNodeStaleRead(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 4)
	b.read(1, "k0", "b", 5, 6)
	b.readKeyOn(2, "n1", "k0", "b", 5, 6)
	b.readKeyOn(3, "n2", "k0", "a", 5, 7)
	report := Register{}.Check(b.build())
	expectKinds(t, report, RegisterNodeStale)
	details := report.Violations[0].Details
	if details["node"] != "n2" || details["value"] != "a" {
		t.Fatalf("details %+v", details)
	}
	if len(report.Undecided) != 1 || !report.Undecided[RegisterReplicasAgree] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterNodeReadFindsNoValue(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.readKeyOn(1, "n3", "k0", nil, 3, 4)
	report := Register{}.Check(b.build())
	expectKinds(t, report, RegisterNodeStale)
	if report.Violations[0].Details["node"] != "n3" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
	if !report.Decided || !report.Undecided[RegisterStale] || !report.Undecided[RegisterLinearizable] {
		t.Fatalf("report %+v", report)
	}
}

func TestRegisterNodeReadConcurrentWithWriteIsNotStale(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 6)
	b.readKeyOn(1, "n1", "k0", "a", 4, 5)
	b.read(2, "k0", "b", 7, 8)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if len(report.Undecided) != 1 || !report.Undecided[RegisterReplicasAgree] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterGeneratorUsesKnownValuesForCAS(t *testing.T) {
	generator := Register{}.Generator(0)
	generator.Observe(Op{F: RegisterWrite, K: "k1", V: "x"}, history.OK, nil)
	counter := 0
	value := func() string { counter++; return "v" }
	op := generator.Next(func(site string, n uint64) uint64 {
		if site == "register.op" {
			return 3
		}
		return 1
	}, value)
	if op.F != RegisterCAS || op.K != "k1" || op.V.([]string)[0] != "x" || counter != 1 {
		t.Fatalf("op %+v", op)
	}
}

func TestRegisterGeneratorWithoutReads(t *testing.T) {
	generator := Register{NoReads: true}.Generator(0)
	generator.Observe(Op{F: RegisterWrite, K: "k0", V: "x"}, history.OK, nil)
	seen := map[string]bool{}
	for choice := uint64(0); choice < 2; choice++ {
		op := generator.Next(func(site string, n uint64) uint64 {
			if site == "register.op" {
				return choice % n
			}
			return 0
		}, func() string { return "v" })
		seen[op.F] = true
	}
	if !seen[RegisterWrite] || !seen[RegisterCAS] || seen[RegisterReadF] {
		t.Fatalf("operations %v", seen)
	}
}

func TestRegisterMonotonicAcceptsStaleRead(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 4)
	b.read(1, "k0", "a", 5, 6)
	b.read(1, "k0", "a", 7, 8)
	b.read(2, "k0", nil, 5, 6)
	b.read(1, "k0", "b", 9, 10)
	report := Register{Reads: MonotonicReads}.Check(b.build())
	expectKinds(t, report)
	if report.Undecided[RegisterMonotonic] || report.Undecided[RegisterWritesLinear] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterMonotonicFindsOlderValue(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 4)
	b.read(1, "k0", "b", 5, 6)
	b.read(1, "k0", "a", 7, 8)
	report := Register{Reads: MonotonicReads}.Check(b.build())
	expectKinds(t, report, RegisterMonotonic)
	if report.Violations[0].Details["previous_value"] != "b" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestRegisterMonotonicFindsMissingValue(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.read(1, "k0", "a", 3, 4)
	b.read(1, "k0", nil, 5, 6)
	expectKinds(t, Register{Reads: MonotonicReads}.Check(b.build()), RegisterMonotonic)
}

func TestRegisterMonotonicAcceptsConcurrentWrites(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 6)
	b.write(2, "k0", "b", 2, history.OK, 5)
	b.read(1, "k0", "b", 7, 8)
	b.read(1, "k0", "a", 9, 10)
	expectKinds(t, Register{Reads: MonotonicReads}.Check(b.build()))
}

func TestRegisterMonotonicChecksCASOutcomes(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.cas(1, "k0", "a", "b", 3, history.OK, 4)
	b.cas(2, "k0", "a", "c", 5, history.OK, 6)
	expectKinds(t, Register{Reads: MonotonicReads}.Check(b.build()), RegisterWritesLinear)
}

func TestRegisterMonotonicKeepsNodeCheck(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.readKeyOn(1, "n3", "k0", nil, 3, 4)
	expectKinds(t, Register{Reads: MonotonicReads}.Check(b.build()), RegisterNodeStale)
}

func (b *builder) load(p int, token string, invoke int64, outcome history.Type, complete int64) {
	b.op(drive, p, RegisterLoad, "", token, invoke, outcome, nil, complete)
}

func TestRegisterLoadSetsEveryKey(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k3", "b", 3, history.OK, 4)
	b.load(1, "t", 5, history.OK, 6)
	for index := range registerKeys {
		key := registerKey(index)
		b.read(2, key, LoadValue("t", key), 7+int64(2*index), 8+int64(2*index))
	}
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if !report.Facts[RegisterWriteAcked] {
		t.Fatalf("report %+v", report)
	}
}

func TestRegisterReadBeforeAcknowledgedLoadIsStale(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.load(1, "t", 3, history.OK, 4)
	b.read(2, "k0", "a", 5, 6)
	expectKinds(t, Register{}.Check(b.build()), RegisterStale, RegisterLinearizable)
}

func TestRegisterIndeterminateLoadMayOrMayNotApply(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.load(1, "t", 3, history.Info, 0)
	b.read(2, "k0", "a", 5, 6)
	b.read(2, "k0", LoadValue("t", "k0"), 7, 8)
	expectKinds(t, Register{}.Check(b.build()))
}

func TestRegisterFailedLoadChangesNothing(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.load(1, "t", 3, history.Fail, 4)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	expectKinds(t, Register{}.Check(b.build()), RegisterLinearizable)
}

func TestRegisterGeneratorDrawsLoadsAndLearnsTheirValues(t *testing.T) {
	generator := Register{Loads: 8}.Generator(0)
	op := generator.Next(func(site string, n uint64) uint64 {
		if site == "register.load" && n == 8 {
			return 0
		}
		return 1
	}, func() string { return "t" })
	if op.F != RegisterLoad || op.V != "t" {
		t.Fatalf("op %+v", op)
	}
	generator.Observe(op, history.OK, nil)
	next := generator.Next(func(site string, n uint64) uint64 {
		if site == "register.op" {
			return 3
		}
		return 1
	}, func() string { return "u" })
	if next.F != RegisterCAS || next.V.([]string)[0] != LoadValue("t", next.K) {
		t.Fatalf("op %+v", next)
	}
}

func TestRegisterReadMissingAShownLoadOnAnotherKeyIsNotAtomic(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "b", 1, history.OK, 2)
	b.load(1, "t", 3, history.OK, 20)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.read(3, "k1", "b", 7, 8)
	expectKinds(t, Register{Loads: 8}.Check(b.build()), RegisterLoadAtomic)
}

func TestRegisterIndeterminateLoadShownOnOneKeyAndMissedOnAnotherIsNotAtomic(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "b", 1, history.OK, 2)
	b.load(1, "t", 3, history.Info, 0)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.read(3, "k1", "b", 7, 8)
	b.read(3, "k1", "b", 9, 10)
	expectKinds(t, Register{Loads: 8}.Check(b.build()), RegisterLoadAtomic)
}

func TestRegisterWriteAfterOrDuringALoadOverwritesItsValue(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "b", 1, history.OK, 2)
	b.write(4, "k2", "c", 2, history.OK, 9)
	b.load(1, "t", 3, history.OK, 4)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.write(0, "k1", "d", 7, history.OK, 8)
	b.read(3, "k1", "d", 10, 11)
	b.read(3, "k2", "c", 12, 13)
	report := Register{Loads: 8}.Check(b.build())
	expectKinds(t, report)
	if _, undecided := report.Undecided[RegisterLoadAtomic]; undecided || !report.Decided {
		t.Fatalf("report %+v", report)
	}
	if !slices.Contains(Register{Loads: 8}.Kinds(), RegisterLoadAtomic) {
		t.Fatal("loads add no cross-key kind")
	}
}

func TestRegisterCASExpectingThePreLoadValueAcrossALoadFails(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.cas(2, "k0", "a", "x", 3, history.Fail, 8)
	b.load(1, "t", 4, history.OK, 5)
	b.read(3, "k0", LoadValue("t", "k0"), 9, 10)
	expectKinds(t, Register{Loads: 8}.Check(b.build()))
}

func TestRegisterLoadsAndWatchesAreRefusedTogether(t *testing.T) {
	if err := Validate(Register{Watch: 4, Loads: 8}); err == nil {
		t.Fatal("loads with watches validated")
	}
	for _, r := range []Register{{}, {Watch: 4}, {Loads: 8}} {
		if err := Validate(r); err != nil {
			t.Fatalf("%+v: %v", r, err)
		}
	}
}

func TestRegisterGeneratorWithoutLoadsDrawsNoLoadSite(t *testing.T) {
	var sites []string
	state := uint64(1)
	generator := Register{}.Generator(0)
	for range 200 {
		op := generator.Next(func(site string, n uint64) uint64 {
			sites = append(sites, site)
			state = state*6364136223846793005 + 1442695040888963407
			return (state >> 33) % n
		}, func() string { return fmt.Sprintf("v%d", len(sites)) })
		generator.Observe(op, history.OK, nil)
	}
	for _, site := range sites {
		if site != "register.key" && site != "register.op" {
			t.Fatalf("draw sites %v", sites)
		}
	}
}

func TestRegisterReadWithNoValueAfterAShownLoadIsNotAtomic(t *testing.T) {
	b := newBuilder(t)
	b.load(1, "t", 1, history.OK, 20)
	b.read(2, "k0", LoadValue("t", "k0"), 3, 4)
	b.read(3, "k1", nil, 5, 6)
	expectKinds(t, Register{Loads: 8}.Check(b.build()), RegisterLoadAtomic)
}

func TestRegisterReadOfTheEarlierOfTwoLoadsAfterTheLaterIsShownIsNotAtomic(t *testing.T) {
	b := newBuilder(t)
	b.load(1, "s", 1, history.OK, 2)
	b.load(1, "t", 3, history.OK, 20)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.read(3, "k1", LoadValue("s", "k1"), 7, 8)
	expectKinds(t, Register{Loads: 8}.Check(b.build()), RegisterLoadAtomic)
}

func TestRegisterReadOfTheLaterOfTwoLoadsIsAtomic(t *testing.T) {
	b := newBuilder(t)
	b.load(1, "s", 1, history.OK, 2)
	b.load(1, "t", 3, history.OK, 20)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.read(3, "k1", LoadValue("t", "k1"), 7, 8)
	expectKinds(t, Register{Loads: 8}.Check(b.build()))
}

func TestRegisterValueOfAnIndeterminateWriteIsNotCountedAgainstALoad(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "b", 1, history.Info, 0)
	b.load(1, "t", 3, history.OK, 20)
	b.read(2, "k0", LoadValue("t", "k0"), 5, 6)
	b.read(3, "k1", "b", 7, 8)
	expectKinds(t, Register{Loads: 8}.Check(b.build()))
}
