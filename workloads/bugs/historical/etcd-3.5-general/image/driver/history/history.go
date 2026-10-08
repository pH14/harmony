// SPDX-License-Identifier: AGPL-3.0-or-later

// Package history is the journal of the general etcd workload and the rule
// that decides whether a member's view of the owned keys is explained by it.
//
// A client appends an intent line before it sends a write and an outcome line
// once it knows the result:
//
//	I <op> <kind> <key> <value>      kind is put, del or cas
//	A <op> <revision> <applied>      acknowledged; applied is 1 when it wrote
//	F <op>                           definitely not applied
//
// An intent without an outcome is indeterminate: it may apply at any later
// revision, or never.
package history

import (
	"bufio"
	"fmt"
	"io"
	"sort"
	"strconv"
	"strings"
)

type Kind string

const (
	Put    Kind = "put"
	Delete Kind = "del"
	CAS    Kind = "cas"
)

type Outcome int

const (
	Indeterminate Outcome = iota
	Acknowledged
	Failed
)

type Op struct {
	ID       uint64
	Kind     Kind
	Key      string
	Value    string
	Outcome  Outcome
	Revision int64
	Applied  bool
}

// Writes reports whether the op leaves the key present.
func (o Op) Writes() bool { return o.Kind != Delete }

type Journal struct {
	ops   map[uint64]*Op
	order []uint64
}

func NewJournal() *Journal { return &Journal{ops: map[uint64]*Op{}} }

func IntentLine(id uint64, kind Kind, key, value string) string {
	return fmt.Sprintf("I %d %s %s %s\n", id, kind, key, value)
}

func AckLine(id uint64, revision int64, applied bool) string {
	flag := 0
	if applied {
		flag = 1
	}
	return fmt.Sprintf("A %d %d %d\n", id, revision, flag)
}

func FailLine(id uint64) string { return fmt.Sprintf("F %d\n", id) }

// Read parses complete lines. A torn final line is ignored: its intent was
// not yet sent, or its outcome is unknown and the op stays indeterminate.
func (j *Journal) Read(r io.Reader) error {
	reader := bufio.NewReader(r)
	for {
		line, err := reader.ReadString('\n')
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		j.apply(strings.Fields(line))
	}
}

func (j *Journal) apply(fields []string) {
	if len(fields) < 2 {
		return
	}
	id, err := strconv.ParseUint(fields[1], 10, 64)
	if err != nil {
		return
	}
	switch fields[0] {
	case "I":
		if len(fields) != 5 {
			return
		}
		if _, seen := j.ops[id]; seen {
			return
		}
		j.ops[id] = &Op{ID: id, Kind: Kind(fields[2]), Key: fields[3], Value: fields[4]}
		j.order = append(j.order, id)
	case "A":
		op := j.ops[id]
		if op == nil || len(fields) != 4 {
			return
		}
		revision, err := strconv.ParseInt(fields[2], 10, 64)
		if err != nil || revision < 1 {
			return
		}
		op.Outcome = Acknowledged
		op.Revision = revision
		op.Applied = fields[3] == "1"
	case "F":
		if op := j.ops[id]; op != nil {
			op.Outcome = Failed
		}
	}
}

// Record applies one complete journal line.
func (j *Journal) Record(line string) { j.apply(strings.Fields(line)) }

// Keys returns the journal's keys in sorted order.
func (j *Journal) Keys() []string {
	seen := map[string]bool{}
	for _, id := range j.order {
		seen[j.ops[id].Key] = true
	}
	keys := make([]string, 0, len(seen))
	for key := range seen {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// OnKey returns the key's ops in the order the owner issued them.
func (j *Journal) OnKey(key string) []Op {
	var ops []Op
	for _, id := range j.order {
		if op := j.ops[id]; op.Key == key {
			ops = append(ops, *op)
		}
	}
	return ops
}

// Journals are the clients' journals. Each client numbers its own ops, so
// ops are looked up per journal and never merged by id.
type Journals []*Journal

func (js Journals) Keys() []string {
	seen := map[string]bool{}
	for _, j := range js {
		for _, key := range j.Keys() {
			seen[key] = true
		}
	}
	keys := make([]string, 0, len(seen))
	for key := range seen {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

func (js Journals) OnKey(key string) []Op {
	var ops []Op
	for _, j := range js {
		ops = append(ops, j.OnKey(key)...)
	}
	return ops
}

// Value is one key as a member reports it; Present is false for an absent
// key.
type Value struct {
	Present     bool
	Data        string
	ModRevision int64
}

// Explain checks one key's state on a member whose read reported revision r.
// It returns an empty string when the journal explains the state.
//
// A present value whose mod_revision matches an acknowledged write must carry
// that write's value; one that matches none must carry the value of an
// indeterminate write. Either way no acknowledged write to the key may sit in
// (mod_revision, r], because the member would then be missing it. An absent
// key is explained when the newest acknowledged write at or below r is a
// delete, when there is none, or when an indeterminate delete exists.
func Explain(ops []Op, r int64, seen Value) string {
	var latest *Op
	for i := range ops {
		op := &ops[i]
		if op.Outcome == Acknowledged && op.Applied && op.Revision <= r {
			if latest == nil || op.Revision > latest.Revision {
				latest = op
			}
		}
	}
	if !seen.Present {
		if latest == nil || !latest.Writes() {
			return ""
		}
		for _, op := range ops {
			if op.Outcome == Indeterminate && op.Kind == Delete {
				return ""
			}
		}
		return fmt.Sprintf("absent at revision %d, but acknowledged %s at revision %d wrote %q",
			r, latest.Kind, latest.Revision, latest.Value)
	}
	if seen.ModRevision > r {
		return fmt.Sprintf("mod_revision %d is above the read revision %d", seen.ModRevision, r)
	}
	for _, op := range ops {
		if op.Outcome == Acknowledged && op.Applied && op.Revision > seen.ModRevision && op.Revision <= r {
			return fmt.Sprintf("value from revision %d, but acknowledged %s at revision %d is at or below the read revision %d",
				seen.ModRevision, op.Kind, op.Revision, r)
		}
	}
	for _, op := range ops {
		if op.Outcome == Acknowledged && op.Applied && op.Revision == seen.ModRevision {
			if !op.Writes() || op.Value != seen.Data {
				return fmt.Sprintf("revision %d holds %q, but the acknowledged %s there wrote %q",
					seen.ModRevision, seen.Data, op.Kind, op.Value)
			}
			return ""
		}
	}
	for _, op := range ops {
		if op.Outcome == Indeterminate && op.Writes() && op.Value == seen.Data {
			return ""
		}
	}
	return fmt.Sprintf("value %q at revision %d matches no acknowledged or indeterminate write", seen.Data, seen.ModRevision)
}

// Latest returns the newest acknowledged write and whether the key has any
// indeterminate write, which makes a linearizable read uncheckable.
func Latest(ops []Op) (*Op, bool) {
	var latest *Op
	uncertain := false
	for i := range ops {
		op := &ops[i]
		switch {
		case op.Outcome == Indeterminate:
			uncertain = true
		case op.Outcome == Acknowledged && op.Applied:
			if latest == nil || op.Revision > latest.Revision {
				latest = op
			}
		}
	}
	return latest, uncertain
}
