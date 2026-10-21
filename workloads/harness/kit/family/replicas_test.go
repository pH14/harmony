// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"testing"

	"harmony.local/harness/kit/history"
)

const check = "check-1.jsonl"

func held(value string, revision uint64) Copy {
	return Copy{Value: &value, Meta: map[string]uint64{"revision": revision}}
}

func (b *builder) replicas(round int, node string, copies map[string]Copy, invoke, complete int64) {
	query := ReplicaQuery{Round: round, Keys: []string{"k0", "k1"}}
	if copies == nil {
		b.opOn(check, 10+int(node[len(node)-1]), RegisterReplicas, "", node, query, invoke, history.Fail, map[string]string{"error": "down"}, complete)
		return
	}
	b.opOn(check, 10+int(node[len(node)-1]), RegisterReplicas, "", node, query, invoke, history.OK, copies, complete)
}

func (b *builder) rounds(from int64, copies func(round int) map[string]map[string]Copy) {
	for round := range replicaRounds {
		at := from + int64(10*round)
		for node, held := range copies(round) {
			b.replicas(round, node, held, at, at+5)
		}
	}
}

func TestReplicasThatAgreePass(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.replicas(0, "n1", map[string]Copy{"k0": held("a", 4)}, 10, 12)
	b.replicas(0, "n2", map[string]Copy{"k0": held("a", 4)}, 10, 13)
	b.replicas(0, "n3", nil, 10, 14)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if !report.Decided || report.Undecided[RegisterReplicasAgree] {
		t.Fatalf("report %+v", report)
	}
}

func TestReplicaRevisionBehindForEqualValue(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.write(0, "k0", "b", 3, history.OK, 4)
	b.rounds(10, func(round int) map[string]map[string]Copy {
		b.write(1, "k0", "c", int64(11+10*round), history.OK, int64(12+10*round))
		return map[string]map[string]Copy{
			"n1": {"k0": held("b", 8)},
			"n2": {"k0": held("b", 8)},
			"n3": {"k0": held("b", 7)},
		}
	})
	report := Register{}.Check(b.build())
	expectKinds(t, report, RegisterReplicasAgree)
	details := report.Violations[0].Details
	if details["key"] != "k0" || details["differs"] != "metadata" {
		t.Fatalf("details %+v", details)
	}
}

func TestReplicaValueDiffersOnQuietKey(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "a", 1, history.OK, 2)
	b.write(0, "k1", "b", 3, history.Info, 4)
	b.rounds(10, func(int) map[string]map[string]Copy {
		return map[string]map[string]Copy{
			"n1": {"k1": held("b", 2)},
			"n2": {"k1": held("a", 1)},
		}
	})
	report := Register{}.Check(b.build())
	expectKinds(t, report, RegisterReplicasAgree)
	if report.Violations[0].Details["differs"] != "value" {
		t.Fatalf("details %+v", report.Violations[0].Details)
	}
}

func TestReplicaValueDiffersWhileKeyIsWritten(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k1", "a", 1, history.OK, 2)
	b.rounds(10, func(round int) map[string]map[string]Copy {
		b.write(1, "k1", "b", int64(9+10*round), history.OK, int64(13+10*round))
		return map[string]map[string]Copy{
			"n1": {"k1": held("b", 2)},
			"n2": {"k1": held("a", 1)},
		}
	})
	report := Register{}.Check(b.build())
	expectKinds(t, report)
}

func TestReplicaAbsentFromOneNode(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.rounds(10, func(int) map[string]map[string]Copy {
		return map[string]map[string]Copy{
			"n1": {"k0": held("a", 2)},
			"n2": {},
		}
	})
	expectKinds(t, Register{}.Check(b.build()), RegisterReplicasAgree)
}

func TestReplicasThatSettlePass(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.rounds(10, func(round int) map[string]map[string]Copy {
		behind := held("a", 1)
		if round == replicaRounds-1 {
			behind = held("a", 2)
		}
		return map[string]map[string]Copy{
			"n1": {"k0": held("a", 2)},
			"n2": {"k0": behind},
		}
	})
	expectKinds(t, Register{}.Check(b.build()))
}

func TestReplicaRoundsCutShortDecideNothing(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.replicas(0, "n1", map[string]Copy{"k0": held("a", 2)}, 10, 12)
	b.replicas(0, "n2", map[string]Copy{"k0": held("a", 1)}, 10, 12)
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if report.Undecided[RegisterReplicasAgree] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestOneAnsweringNodeLeavesReplicasUndecided(t *testing.T) {
	b := newBuilder(t)
	b.write(0, "k0", "a", 1, history.OK, 2)
	b.rounds(10, func(int) map[string]map[string]Copy {
		return map[string]map[string]Copy{"n1": {"k0": held("a", 2)}, "n2": nil}
	})
	report := Register{}.Check(b.build())
	expectKinds(t, report)
	if !report.Undecided[RegisterReplicasAgree] {
		t.Fatalf("undecided %v", report.Undecided)
	}
}

func TestAgreeComparesValuesAndMetadata(t *testing.T) {
	same := map[string]map[string]Copy{"n1": {"k0": held("a", 2)}, "n2": {"k0": held("a", 2)}}
	if !(Register{}).Agree(same) {
		t.Fatal("equal copies disagree")
	}
	values := map[string]map[string]Copy{"n1": {"k0": held("a", 2)}, "n2": {"k0": held("b", 3)}}
	if (Register{}).Agree(values) {
		t.Fatal("different values agree")
	}
	a := "a"
	plain := map[string]map[string]Copy{"n1": {"k0": {Value: &a}}, "n2": {"k0": {Value: &a}}}
	if !(Register{}).Agree(plain) {
		t.Fatal("values without metadata disagree")
	}
}
