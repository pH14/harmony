// SPDX-License-Identifier: AGPL-3.0-or-later

package history

import (
	"strings"
	"testing"
)

func journal(t *testing.T, lines ...string) *Journal {
	t.Helper()
	j := NewJournal()
	if _, err := j.Read(strings.NewReader(strings.Join(lines, ""))); err != nil {
		t.Fatal(err)
	}
	return j
}

func present(data string, mod int64) Value { return Value{Present: true, Data: data, ModRevision: mod} }

func TestAcknowledgedWritesExplainTheLatestValue(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Put, "k", "b"), AckLine(2, 12, true),
	)
	if got := Explain(j.OnKey("k"), 12, present("b", 12)); got != "" {
		t.Fatal(got)
	}
	if got := Explain(j.OnKey("k"), 11, present("a", 10)); got != "" {
		t.Fatalf("a member behind the second ack is stale, not wrong: %s", got)
	}
}

func TestAMemberMissingAnAcknowledgedWriteIsAViolation(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Put, "k", "b"), AckLine(2, 12, true),
	)
	if got := Explain(j.OnKey("k"), 15, present("a", 10)); got == "" {
		t.Fatal("a member at revision 15 still holding revision 10 lost the write at 12")
	}
	if got := Explain(j.OnKey("k"), 15, Value{}); got == "" {
		t.Fatal("an acknowledged key that is absent was lost")
	}
}

func TestAFailedWriteNeverExplainsAValue(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Put, "k", "b"), FailLine(2),
	)
	if got := Explain(j.OnKey("k"), 20, present("a", 10)); got != "" {
		t.Fatal(got)
	}
	if got := Explain(j.OnKey("k"), 20, present("b", 14)); got == "" {
		t.Fatal("a definitely failed write cannot appear")
	}
}

func TestAnIndeterminateWriteMayOrMayNotApply(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Put, "k", "b"),
	)
	for _, seen := range []Value{present("a", 10), present("b", 14)} {
		if got := Explain(j.OnKey("k"), 20, seen); got != "" {
			t.Fatalf("%v: %s", seen, got)
		}
	}
	if got := Explain(j.OnKey("k"), 20, present("c", 14)); got == "" {
		t.Fatal("a value no write produced is a violation")
	}
}

func TestATimedOutWriteThatLandsAfterALaterAckIsStillExplained(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"),
		IntentLine(2, Put, "k", "b"), AckLine(2, 12, true),
	)
	if got := Explain(j.OnKey("k"), 20, present("a", 13)); got != "" {
		t.Fatalf("the timed-out put may have committed after the acknowledged one: %s", got)
	}
}

func TestDeletesExplainAbsence(t *testing.T) {
	acked := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Delete, "k", "-"), AckLine(2, 12, true),
	)
	if got := Explain(acked.OnKey("k"), 12, Value{}); got != "" {
		t.Fatal(got)
	}
	if got := Explain(acked.OnKey("k"), 12, present("a", 10)); got == "" {
		t.Fatal("a value that an acknowledged delete removed was resurrected")
	}
	pending := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Delete, "k", "-"),
	)
	if got := Explain(pending.OnKey("k"), 20, Value{}); got != "" {
		t.Fatal(got)
	}
	noop := journal(t, IntentLine(1, Delete, "k", "-"), AckLine(1, 4, false))
	if got := Explain(noop.OnKey("k"), 20, Value{}); got != "" {
		t.Fatal(got)
	}
}

func TestACompareThatFailedWroteNothing(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, CAS, "k", "b"), AckLine(2, 11, false),
	)
	if got := Explain(j.OnKey("k"), 11, present("a", 10)); got != "" {
		t.Fatal(got)
	}
	if got := Explain(j.OnKey("k"), 11, present("b", 11)); got == "" {
		t.Fatal("a compare that failed cannot appear")
	}
}

func TestAValueMustMatchTheWriteAtItsRevision(t *testing.T) {
	j := journal(t, IntentLine(1, Put, "k", "a"), AckLine(1, 10, true))
	if got := Explain(j.OnKey("k"), 10, present("z", 10)); got == "" {
		t.Fatal("revision 10 wrote a, not z")
	}
	if got := Explain(j.OnKey("k"), 10, present("a", 11)); got == "" {
		t.Fatal("a mod_revision above the read revision is impossible")
	}
}

func TestATornLineIsIgnoredAndLeavesTheOpIndeterminate(t *testing.T) {
	j := journal(t, IntentLine(1, Put, "k", "a"), "A 1 10")
	ops := j.OnKey("k")
	if len(ops) != 1 || ops[0].Outcome != Indeterminate {
		t.Fatalf("%+v", ops)
	}
	j = journal(t, "I 1 put k")
	if len(j.Keys()) != 0 {
		t.Fatal("a torn intent was never sent")
	}
}

func TestLatestReportsUncertainty(t *testing.T) {
	j := journal(t,
		IntentLine(1, Put, "k", "a"), AckLine(1, 10, true),
		IntentLine(2, Put, "k", "b"),
	)
	latest, uncertain := Latest(j.OnKey("k"))
	if latest == nil || latest.Value != "a" || !uncertain {
		t.Fatalf("%+v %v", latest, uncertain)
	}
}

func TestAnUnknownKeyIsAViolation(t *testing.T) {
	if got := Explain(nil, 10, present("x", 3)); got == "" {
		t.Fatal("a key no client wrote cannot exist")
	}
	if got := Explain(nil, 10, Value{}); got != "" {
		t.Fatal(got)
	}
}

func TestClientsNumberTheirOpsIndependently(t *testing.T) {
	first := journal(t, IntentLine(1, Put, "gen/1/0", "a"), AckLine(1, 10, true))
	second := journal(t, IntentLine(1, Put, "gen/2/0", "b"), AckLine(1, 11, true))
	all := Journals{first, second}
	if got := Explain(all.OnKey("gen/2/0"), 11, present("b", 11)); got != "" {
		t.Fatal(got)
	}
	if got := Explain(all.OnKey("gen/1/0"), 11, present("a", 10)); got != "" {
		t.Fatal(got)
	}
	if keys := all.Keys(); len(keys) != 2 {
		t.Fatal(keys)
	}
}

func TestReadCountsOnlyCompleteLines(t *testing.T) {
	whole := IntentLine(1, Put, "k", "a") + AckLine(1, 10, true)
	j := NewJournal()
	complete, err := j.Read(strings.NewReader(whole + "I 2 put k"))
	if err != nil {
		t.Fatal(err)
	}
	if complete != int64(len(whole)) {
		t.Fatalf("complete length %d, want %d", complete, len(whole))
	}
	if ops := j.OnKey("k"); len(ops) != 1 || ops[0].Outcome != Acknowledged {
		t.Fatalf("the torn intent became an op: %+v", ops)
	}
}
