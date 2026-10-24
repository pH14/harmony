// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"encoding/json"
	"maps"
	"sort"
	"time"

	"harmony.local/harness/kit/history"
)

const RegisterReplicasAgree = "register: every node holds the same value and metadata for each key"

const RegisterReplicas = "replicas"

const (
	replicaRounds = 3
	replicaSettle = 500 * time.Millisecond
)

type Copy struct {
	Value *string           `json:"value"`
	Meta  map[string]uint64 `json:"meta,omitempty"`
}

type ReplicaQuery struct {
	Round int      `json:"round"`
	Keys  []string `json:"keys"`
}

type Replicated interface {
	Settle() (rounds int, pause time.Duration)
	Replicas(round int) Op
	Disagree(h *history.History, journal string, round int) bool
}

func (r Register) Settle() (int, time.Duration) {
	rounds, pause := r.Rounds, r.Pause
	if rounds <= 0 {
		rounds = replicaRounds
	}
	if pause <= 0 {
		pause = replicaSettle
	}
	return rounds, pause
}

func (r Register) Replicas(round int) Op {
	keys := make([]string, r.keys())
	for index := range keys {
		keys[index] = registerKey(index)
	}
	return Op{F: RegisterReplicas, V: ReplicaQuery{Round: round, Keys: keys}}
}

func (r Register) Disagree(h *history.History, journal string, round int) bool {
	rounds, _ := r.Settle()
	at := collectRounds(h, rounds)[journal][round]
	if at == nil {
		return false
	}
	writes := map[string][]registerOp{}
	for index := range h.Ops {
		op := &h.Ops[index]
		if decoded, ok := decodeRegister(op); ok && op.F != RegisterReadF {
			writes[op.K] = append(writes[op.K], decoded)
		}
	}
	return len(at.disagreements(writes)) > 0
}

type disagreement struct {
	key     string
	differs string
	copies  map[string]Copy
}

func disagreements(copies map[string]map[string]Copy, quiet func(key string) bool) []disagreement {
	nodes := sortedKeys(copies)
	if len(nodes) < 2 {
		return nil
	}
	keySet := map[string]bool{}
	for _, held := range copies {
		for key := range held {
			keySet[key] = true
		}
	}
	var found []disagreement
	for _, key := range sortedKeys(keySet) {
		byNode := map[string]Copy{}
		values := map[string]bool{}
		metadata := false
		first := map[string]Copy{}
		for _, node := range nodes {
			held := copies[node][key]
			byNode[node] = held
			value := "\x00absent"
			if held.Value != nil {
				value = *held.Value
				if earlier, ok := first[value]; ok && !maps.Equal(earlier.Meta, held.Meta) {
					metadata = true
				} else if !ok {
					first[value] = held
				}
			}
			values[value] = true
		}
		switch {
		case metadata:
			found = append(found, disagreement{key: key, differs: "metadata", copies: byNode})
		case len(values) > 1 && quiet(key):
			found = append(found, disagreement{key: key, differs: "value", copies: byNode})
		}
	}
	return found
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

type replicaRound struct {
	start, end int64
	copies     map[string]map[string]Copy
}

func (round *replicaRound) disagreements(writes map[string][]registerOp) []disagreement {
	return disagreements(round.copies, func(key string) bool {
		return !writtenDuring(writes[key], round.start, round.end)
	})
}

func collectRounds(h *history.History, rounds int) map[string]map[int]*replicaRound {
	byJournal := map[string]map[int]*replicaRound{}
	for index := range h.Ops {
		op := &h.Ops[index]
		if op.F != RegisterReplicas {
			continue
		}
		var query ReplicaQuery
		if json.Unmarshal(op.Input, &query) != nil || query.Round < 0 || query.Round >= rounds {
			continue
		}
		if byJournal[op.Journal] == nil {
			byJournal[op.Journal] = map[int]*replicaRound{}
		}
		round := byJournal[op.Journal][query.Round]
		if round == nil {
			round = &replicaRound{start: op.Invoke, end: op.Ended, copies: map[string]map[string]Copy{}}
			byJournal[op.Journal][query.Round] = round
		}
		round.start = min(round.start, op.Invoke)
		round.end = max(round.end, op.Ended)
		var held map[string]Copy
		if op.Type == history.OK && op.Node != "" && json.Unmarshal(op.Output, &held) == nil {
			round.copies[op.Node] = held
		}
	}
	return byJournal
}

func (r Register) checkReplicas(h *history.History, writes map[string][]registerOp, report *Report, found *collector) {
	rounds, _ := r.Settle()
	byJournal := collectRounds(h, rounds)
	for _, journal := range sortedKeys(byJournal) {
		journalRounds := byJournal[journal]
		compared, settled := 0, false
		for _, round := range journalRounds {
			if len(round.copies) >= 2 {
				compared++
				settled = settled || len(round.disagreements(writes)) == 0
			}
		}
		if settled || compared == rounds {
			report.Decided = true
			delete(report.Undecided, RegisterReplicasAgree)
		}
		var last []disagreement
		every := len(journalRounds) == rounds
		for at := 0; at < rounds && every; at++ {
			last = journalRounds[at].disagreements(writes)
			every = len(last) > 0
		}
		if !every {
			continue
		}
		for _, d := range last {
			found.add(RegisterReplicasAgree, func() map[string]any {
				return map[string]any{
					"key":     d.key,
					"differs": d.differs,
					"copies":  d.copies,
					"journal": journal,
					"rounds":  rounds,
				}
			})
		}
	}
}

func writtenDuring(writes []registerOp, start, end int64) bool {
	for _, write := range writes {
		if write.input.f != RegisterReadF && write.op.Invoke <= end && write.op.Ended >= start {
			return true
		}
	}
	return false
}
