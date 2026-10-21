// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"encoding/json"
	"fmt"
	"sort"

	"github.com/anishathalye/porcupine"

	"harmony.local/harness/kit/history"
)

const (
	RegisterLinearizable = "register: each key's history is linearizable"
	RegisterStale        = "register: no read returns a value older than an acknowledged earlier write"
	RegisterNodeStale    = "register: no node's read returns a value older than an acknowledged earlier write"
	RegisterWriteAcked   = "register: a write was acknowledged"
)

const (
	RegisterWrite = "write"
	RegisterReadF = "read"
	RegisterCAS   = "cas"
)

const (
	registerKeys  = 5
	registerSteps = 1 << 16
)

type Register struct {
	Keys  int
	NoCAS bool
	Steps int
}

func (Register) Name() string { return "register" }

func (Register) Kinds() []string {
	return []string{RegisterLinearizable, RegisterStale, RegisterNodeStale}
}

func (Register) Observations() []string { return []string{RegisterWriteAcked} }

func (r Register) keys() int {
	if r.Keys > 0 {
		return r.Keys
	}
	return registerKeys
}

func (r Register) steps() int {
	if r.Steps > 0 {
		return r.Steps
	}
	return registerSteps
}

func (r Register) Read() Op { return Op{F: RegisterReadF, K: "k0"} }

func (r Register) Generator(int) Generator {
	return &registerGenerator{keys: r.keys(), cas: !r.NoCAS, known: map[string]string{}}
}

func (Register) Recorder(*history.History) Recorder { return plainRecorder{} }

type registerGenerator struct {
	keys  int
	cas   bool
	known map[string]string
}

func (g *registerGenerator) Next(draw Draw, value func() string) Op {
	key := fmt.Sprintf("k%d", draw(uint64(g.keys)))
	switch draw(4) {
	case 0, 1:
		return Op{F: RegisterReadF, K: key}
	case 3:
		if old, ok := g.known[key]; ok && g.cas {
			return Op{F: RegisterCAS, K: key, V: []string{old, value()}}
		}
	}
	return Op{F: RegisterWrite, K: key, V: value()}
}

func (g *registerGenerator) Observe(op Op, outcome history.Type, result any) {
	if outcome != history.OK {
		return
	}
	switch op.F {
	case RegisterWrite:
		if value, ok := op.V.(string); ok {
			g.known[op.K] = value
		}
	case RegisterCAS:
		if pair, ok := op.V.([]string); ok && len(pair) == 2 {
			g.known[op.K] = pair[1]
		}
	case RegisterReadF:
		if value, ok := result.(string); ok {
			g.known[op.K] = value
		}
	}
}

type registerInput struct {
	f, value, old string
}

type registerOutput struct {
	value   string
	unknown bool
}

type registerOp struct {
	op     *history.Op
	input  registerInput
	output registerOutput
}

func decodeRegister(op *history.Op) (registerOp, bool) {
	decoded := registerOp{op: op, input: registerInput{f: op.F}}
	switch op.F {
	case RegisterWrite:
		if json.Unmarshal(op.Input, &decoded.input.value) != nil || decoded.input.value == "" {
			return decoded, false
		}
	case RegisterCAS:
		var pair []string
		if json.Unmarshal(op.Input, &pair) != nil || len(pair) != 2 || pair[1] == "" {
			return decoded, false
		}
		decoded.input.old, decoded.input.value = pair[0], pair[1]
	case RegisterReadF:
		if op.Type != history.OK {
			return decoded, false
		}
		var value *string
		if json.Unmarshal(op.Output, &value) != nil {
			return decoded, false
		}
		if value != nil {
			decoded.output.value = *value
		}
	default:
		return decoded, false
	}
	decoded.output.unknown = op.Type == history.Info
	return decoded, op.Type != history.Fail
}

type stepBudget struct {
	left      int
	exhausted bool
}

func registerModel(budget *stepBudget) porcupine.Model {
	return porcupine.Model{
		Init: func() interface{} { return "" },
		Step: func(state, input, output interface{}) (bool, interface{}) {
			if budget.left == 0 {
				budget.exhausted = true
				return false, state
			}
			budget.left--
			current := state.(string)
			in := input.(registerInput)
			out := output.(registerOutput)
			switch in.f {
			case RegisterReadF:
				return out.value == current, current
			case RegisterWrite:
				return true, in.value
			case RegisterCAS:
				if current == in.old {
					return true, in.value
				}
				return out.unknown, current
			}
			return false, current
		},
		DescribeOperation: func(input, output interface{}) string {
			in := input.(registerInput)
			out := output.(registerOutput)
			switch in.f {
			case RegisterReadF:
				return fmt.Sprintf("read() -> %q", out.value)
			case RegisterCAS:
				return fmt.Sprintf("cas(%q, %q)", in.old, in.value)
			}
			return fmt.Sprintf("write(%q)", in.value)
		},
	}
}

func (r Register) Check(h *history.History) Report {
	report := Report{
		Facts:     map[string]bool{},
		Undecided: map[string]bool{RegisterLinearizable: true, RegisterStale: true, RegisterNodeStale: true},
	}
	byKey := map[string][]registerOp{}
	pinned := map[string][]registerOp{}
	var keys []string
	for index := range h.Ops {
		op := &h.Ops[index]
		decoded, ok := decodeRegister(op)
		if !ok {
			continue
		}
		if _, seen := byKey[op.K]; !seen {
			keys = append(keys, op.K)
			byKey[op.K] = nil
		}
		onNode := op.Node != "" && op.F == RegisterReadF
		if onNode {
			pinned[op.K] = append(pinned[op.K], decoded)
		} else {
			byKey[op.K] = append(byKey[op.K], decoded)
		}
		if op.Type != history.OK {
			continue
		}
		switch {
		case op.F != RegisterReadF:
			report.Facts[RegisterWriteAcked] = true
		case onNode:
			report.Decided = true
			delete(report.Undecided, RegisterNodeStale)
		default:
			report.Decided = true
			delete(report.Undecided, RegisterLinearizable)
			delete(report.Undecided, RegisterStale)
		}
	}
	sort.Strings(keys)
	var found collector
	for _, key := range keys {
		ops := byKey[key]
		checkStaleReads(key, ops, ops, RegisterStale, &found)
		checkStaleReads(key, ops, pinned[key], RegisterNodeStale, &found)
		linearizable := withoutUnseenIndeterminate(ops)
		if len(linearizable) == 0 {
			continue
		}
		operations := make([]porcupine.Operation, 0, len(linearizable))
		for _, op := range linearizable {
			operations = append(operations, porcupine.Operation{
				ClientId: op.op.Process,
				Input:    op.input,
				Call:     op.op.Invoke,
				Output:   op.output,
				Return:   op.op.Complete,
			})
		}
		budget := stepBudget{left: r.steps()}
		result := porcupine.CheckOperations(registerModel(&budget), operations)
		switch {
		case budget.exhausted:
			report.Undecided[RegisterLinearizable] = true
		case !result:
			found.add(RegisterLinearizable, func() map[string]any {
				return map[string]any{"key": key, "operations": len(operations)}
			})
		}
	}
	report.Violations = found.result()
	return report
}

func withoutUnseenIndeterminate(ops []registerOp) []registerOp {
	seen := map[string]bool{}
	writes := map[string]int{}
	for _, op := range ops {
		switch op.input.f {
		case RegisterReadF:
			seen[op.output.value] = true
		case RegisterCAS:
			seen[op.input.old] = true
			writes[op.input.value]++
		case RegisterWrite:
			writes[op.input.value]++
		}
	}
	kept := make([]registerOp, 0, len(ops))
	for _, op := range ops {
		unseen := op.input.f != RegisterReadF && !seen[op.input.value] && writes[op.input.value] == 1
		if op.output.unknown && unseen {
			continue
		}
		kept = append(kept, op)
	}
	return kept
}

func checkStaleReads(key string, ops, reads []registerOp, kind string, found *collector) {
	var writes []registerOp
	byValue := map[string]*history.Op{}
	for _, op := range ops {
		if op.op.Type == history.OK && op.input.f != RegisterReadF {
			writes = append(writes, op)
			byValue[op.input.value] = op.op
		}
	}
	sort.SliceStable(writes, func(i, j int) bool { return writes[i].op.Complete < writes[j].op.Complete })
	latest := make([]*history.Op, len(writes))
	for index, write := range writes {
		latest[index] = write.op
		if index > 0 && latest[index-1].Invoke > write.op.Invoke {
			latest[index] = latest[index-1]
		}
	}
	for _, read := range reads {
		if read.input.f != RegisterReadF {
			continue
		}
		before := sort.Search(len(writes), func(i int) bool { return writes[i].op.Complete >= read.op.Invoke })
		if before == 0 {
			continue
		}
		newer := latest[before-1]
		value := read.output.value
		if value == "" {
			found.addFor(kind, read.op.Node, func() map[string]any {
				return map[string]any{"key": key, "read": describe(read.op), "value": nil, "acknowledged_write": describe(newer)}
			})
			continue
		}
		source, ok := byValue[value]
		if !ok || source.Complete >= newer.Invoke {
			continue
		}
		found.addFor(kind, read.op.Node, func() map[string]any {
			return map[string]any{
				"key":                key,
				"read":               describe(read.op),
				"value":              value,
				"value_write":        describe(source),
				"acknowledged_write": describe(newer),
			}
		})
	}
}
