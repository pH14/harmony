// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"fmt"

	"harmony.local/harness/kit/history"
)

type Op struct {
	F    string
	K    string
	Node string
	V    any
}

type Draw func(n uint64) uint64

type Generator interface {
	Next(draw Draw, value func() string) Op
	Observe(op Op, outcome history.Type, result any)
}

type Recorder interface {
	Record(journal *history.Journal, p int, op Op, index int64, result any) error
}

type Violation struct {
	Kind    string         `json:"kind"`
	Details map[string]any `json:"details"`
}

type Report struct {
	Violations []Violation
	Decided    bool
	Undecided  map[string]bool
	Facts      map[string]bool
}

type Family interface {
	Name() string
	Generator(process int) Generator
	Read() Op
	Kinds() []string
	Observations() []string
	Recorder(prior *history.History) Recorder
	Check(h *history.History) Report
}

func ByName(name string) (Family, error) {
	switch name {
	case "log":
		return Log{}, nil
	case "register":
		return Register{}, nil
	}
	return nil, fmt.Errorf("unknown family %q", name)
}

const maxEvidence = 3

type evidenceGroup struct{ kind, node string }

type collector struct {
	violations []Violation
	groups     []evidenceGroup
	counts     map[string]int
	byGroup    map[evidenceGroup]int
}

func (c *collector) add(kind string, details func() map[string]any) {
	c.addFor(kind, "", details)
}

func (c *collector) addFor(kind, node string, details func() map[string]any) {
	if c.counts == nil {
		c.counts = map[string]int{}
		c.byGroup = map[evidenceGroup]int{}
	}
	group := evidenceGroup{kind, node}
	c.counts[kind]++
	c.byGroup[group]++
	if c.byGroup[group] <= maxEvidence {
		evidence := details()
		if node != "" {
			evidence["node"] = node
		}
		c.violations = append(c.violations, Violation{Kind: kind, Details: evidence})
		c.groups = append(c.groups, group)
	}
}

func (c *collector) result() []Violation {
	for index := range c.violations {
		group := c.groups[index]
		c.violations[index].Details["occurrences"] = c.counts[group.kind]
		if group.node != "" {
			c.violations[index].Details["node_occurrences"] = c.byGroup[group]
		}
	}
	return c.violations
}

type plainRecorder struct{}

func (plainRecorder) Record(journal *history.Journal, p int, op Op, index int64, result any) error {
	return journal.Complete(p, history.OK, op.F, op.K, index, result)
}

func describe(op *history.Op) map[string]any {
	description := map[string]any{
		"journal": op.Journal,
		"index":   op.Index,
		"process": op.P,
		"f":       op.F,
		"type":    string(op.Type),
		"invoke":  op.Invoke,
	}
	if op.K != "" {
		description["k"] = op.K
	}
	if op.Node != "" {
		description["node"] = op.Node
	}
	if op.Complete != history.Forever {
		description["complete"] = op.Complete
	}
	return description
}
