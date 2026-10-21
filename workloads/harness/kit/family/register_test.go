// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"fmt"
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
	if len(report.Undecided) != 0 {
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
	if len(report.Undecided) != 0 {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestRegisterGeneratorUsesKnownValuesForCAS(t *testing.T) {
	generator := Register{}.Generator(0)
	generator.Observe(Op{F: RegisterWrite, K: "k1", V: "x"}, history.OK, nil)
	counter := 0
	value := func() string { counter++; return "v" }
	op := generator.Next(func(n uint64) uint64 {
		if n == 4 {
			return 3
		}
		return 1
	}, value)
	if op.F != RegisterCAS || op.K != "k1" || op.V.([]string)[0] != "x" || counter != 1 {
		t.Fatalf("op %+v", op)
	}
}
