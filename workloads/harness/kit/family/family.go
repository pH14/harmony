// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"fmt"

	"harmony.local/harness/kit/history"
)

type Op struct {
	F string
	K string
	V any
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

type collector struct {
	violations []Violation
	counts     map[string]int
}

func (c *collector) add(kind string, details func() map[string]any) {
	if c.counts == nil {
		c.counts = map[string]int{}
	}
	c.counts[kind]++
	if c.counts[kind] <= maxEvidence {
		c.violations = append(c.violations, Violation{Kind: kind, Details: details()})
	}
}

func (c *collector) result() []Violation {
	for index := range c.violations {
		c.violations[index].Details["occurrences"] = c.counts[c.violations[index].Kind]
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
	if op.Complete != history.Forever {
		description["complete"] = op.Complete
	}
	return description
}
