// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"encoding/json"
	"errors"
	"fmt"
	"hash/fnv"
	"sort"
	"time"

	"github.com/anishathalye/porcupine"

	"harmony.local/harness/kit/history"
)

const (
	RegisterLinearizable = "register: each key's history is linearizable"
	RegisterStale        = "register: no read returns a value older than an acknowledged earlier write"
	RegisterNodeStale    = "register: no node's read returns a value older than an acknowledged earlier write"
	RegisterWritesLinear = "register: each key's writes and compare-and-swaps are linearizable"
	RegisterMonotonic    = "register: no client reads a value older than one it read before"
	RegisterWriteAcked   = "register: a write was acknowledged"
	RegisterLoadAtomic   = "register: no read misses a load that a completed read of another key showed"
)

type ReadConsistency int

const (
	LinearizableReads ReadConsistency = iota
	MonotonicReads
)

const (
	RegisterWrite = "write"
	RegisterReadF = "read"
	RegisterCAS   = "cas"
	RegisterLoad  = "load"
)

const (
	registerKeys  = 5
	registerSteps = 1 << 16
)

type Register struct {
	Keys    int
	NoCAS   bool
	NoReads bool
	Steps   int
	Rounds  int
	Pause   time.Duration
	Reads   ReadConsistency
	Watch   uint64
	Loads   uint64
}

func (Register) Name() string { return "register" }

func (r Register) Kinds() []string {
	kinds := []string{RegisterLinearizable, RegisterStale, RegisterNodeStale, RegisterReplicasAgree}
	if r.Reads == MonotonicReads {
		kinds = []string{RegisterWritesLinear, RegisterMonotonic, RegisterNodeStale, RegisterReplicasAgree}
	}
	if r.Watch > 0 {
		kinds = append(kinds, watchKinds()...)
	}
	if r.Loads > 0 && r.Reads == LinearizableReads {
		kinds = append(kinds, RegisterLoadAtomic)
	}
	return kinds
}

func (r Register) Validate() error {
	if r.Watch > 0 && r.Loads > 0 {
		return errors.New("register: loads and watches cannot both be on")
	}
	return nil
}

func (r Register) linearKind() string {
	if r.Reads == MonotonicReads {
		return RegisterWritesLinear
	}
	return RegisterLinearizable
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

func registerKey(index int) string { return fmt.Sprintf("k%d", index) }

func (r Register) Read() Op { return Op{F: RegisterReadF, K: registerKey(0)} }

func (r Register) LoadKeys() []string {
	keys := make([]string, r.keys())
	for index := range keys {
		keys[index] = registerKey(index)
	}
	return keys
}

func LoadValue(token, key string) string {
	hash := fnv.New64a()
	hash.Write([]byte(token))
	return fmt.Sprintf("load-%016x-%s", hash.Sum64(), key)
}

func (r Register) Generator(int) Generator {
	return &registerGenerator{keys: r.keys(), cas: !r.NoCAS, reads: !r.NoReads, watch: r.Watch, loads: r.Loads, known: map[string]string{}}
}

func (Register) Recorder(*history.History) Recorder { return plainRecorder{} }

type registerGenerator struct {
	keys     int
	cas      bool
	reads    bool
	watch    uint64
	loads    uint64
	revision uint64
	known    map[string]string
}

func (g *registerGenerator) Next(draw Draw, value func() string) Op {
	if g.watch > 0 && draw("register.watch", g.watch) == 0 {
		return Op{F: RegisterWatch, V: WatchQuery{From: g.watchFrom(draw)}}
	}
	if g.loads > 0 && draw("register.load", g.loads) == 0 {
		return Op{F: RegisterLoad, V: value()}
	}
	key := registerKey(int(draw("register.key", uint64(g.keys))))
	var choice uint64
	if g.reads {
		choice = draw("register.op", 4)
	} else {
		choice = 2 + draw("register.op", 2)
	}
	switch choice {
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
	g.observeRevision(result)
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
	case RegisterLoad:
		if token, ok := op.V.(string); ok {
			for index := range g.keys {
				key := registerKey(index)
				g.known[key] = LoadValue(token, key)
			}
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

type keyedOp struct {
	key string
	op  registerOp
}

func decodeRegisterKeys(op *history.Op, keys int) []keyedOp {
	if op.F != RegisterLoad {
		decoded, ok := decodeRegister(op)
		if !ok {
			return nil
		}
		return []keyedOp{{key: op.K, op: decoded}}
	}
	var token string
	if op.Type == history.Fail || json.Unmarshal(op.Input, &token) != nil || token == "" {
		return nil
	}
	expanded := make([]keyedOp, keys)
	for index := range expanded {
		key := registerKey(index)
		expanded[index] = keyedOp{key: key, op: registerOp{
			op:     op,
			input:  registerInput{f: RegisterWrite, value: LoadValue(token, key)},
			output: registerOutput{unknown: op.Type == history.Info},
		}}
	}
	return expanded
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
		Undecided: map[string]bool{},
	}
	for _, kind := range r.Kinds() {
		report.Undecided[kind] = true
	}
	byKey := map[string][]registerOp{}
	pinned := map[string][]registerOp{}
	var keys []string
	for index := range h.Ops {
		op := &h.Ops[index]
		expanded := decodeRegisterKeys(op, r.keys())
		for _, keyed := range expanded {
			if _, seen := byKey[keyed.key]; !seen {
				keys = append(keys, keyed.key)
				byKey[keyed.key] = nil
			}
			if op.Node != "" && op.F == RegisterReadF {
				pinned[keyed.key] = append(pinned[keyed.key], keyed.op)
			} else {
				byKey[keyed.key] = append(byKey[keyed.key], keyed.op)
			}
		}
		onNode := op.Node != "" && op.F == RegisterReadF
		if len(expanded) == 0 || op.Type != history.OK {
			continue
		}
		switch {
		case op.F != RegisterReadF:
			report.Facts[RegisterWriteAcked] = true
			if r.Reads == MonotonicReads {
				report.Decided = true
				delete(report.Undecided, RegisterWritesLinear)
			}
		case onNode:
			report.Decided = true
			delete(report.Undecided, RegisterNodeStale)
		default:
			report.Decided = true
			delete(report.Undecided, RegisterLinearizable)
			delete(report.Undecided, RegisterStale)
			delete(report.Undecided, RegisterMonotonic)
		}
	}
	sort.Strings(keys)
	var found collector
	for _, key := range keys {
		ops := byKey[key]
		checked := ops
		if r.Reads == MonotonicReads {
			checkMonotonicReads(key, ops, &found)
			checked = withoutReads(ops)
		} else {
			checkStaleReads(key, ops, ops, RegisterStale, &found)
		}
		checkStaleReads(key, ops, pinned[key], RegisterNodeStale, &found)
		linearizable := withoutUnseenIndeterminate(checked)
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
			report.Undecided[r.linearKind()] = true
			report.UndecidedKeys++
		case !result:
			found.add(r.linearKind(), func() map[string]any {
				return map[string]any{"key": key, "operations": len(operations)}
			})
		}
	}
	if r.Loads > 0 && r.Reads == LinearizableReads {
		r.checkLoadAtomic(h, byKey, &report, &found)
	}
	r.checkReplicas(h, byKey, &report, &found)
	if r.Watch > 0 {
		r.checkWatches(h, &report, &found)
	}
	report.Violations = found.result()
	return report
}

// A read that completed with a load's value on one key shows the load took
// effect before that read returned, so every read of another key that starts
// afterwards must see the load or a write that took effect after it. A value
// whose write was acknowledged before the load was invoked took effect before
// the load, so such a read missed the load on its key.
func (r Register) checkLoadAtomic(h *history.History, byKey map[string][]registerOp, report *Report, found *collector) {
	type shown struct {
		key      string
		complete int64
	}
	type load struct {
		op      *history.Op
		witness []shown
	}
	var loads []*load
	byToken := map[string]*load{}
	for index := range h.Ops {
		op := &h.Ops[index]
		var token string
		if op.F != RegisterLoad || op.Type == history.Fail || json.Unmarshal(op.Input, &token) != nil || token == "" {
			continue
		}
		l := &load{op: op}
		loads = append(loads, l)
		for key := range byKey {
			byToken[LoadValue(token, key)] = l
		}
	}
	if len(loads) == 0 {
		return
	}
	for key, ops := range byKey {
		for _, op := range ops {
			if op.input.f != RegisterReadF || op.op.Type != history.OK {
				continue
			}
			if l, ok := byToken[op.output.value]; ok && op.output.value == LoadValue(tokenOf(l.op), key) {
				l.witness = append(l.witness, shown{key: key, complete: op.op.Complete})
			}
		}
	}
	for _, l := range loads {
		if len(l.witness) > 0 {
			report.Decided = true
			delete(report.Undecided, RegisterLoadAtomic)
			break
		}
	}
	for _, key := range sortedKeys(byKey) {
		ops := byKey[key]
		acknowledged := map[string]*history.Op{}
		for _, op := range ops {
			if op.input.f != RegisterReadF && op.op.Type == history.OK {
				acknowledged[op.input.value] = op.op
			}
		}
	reads:
		for _, read := range ops {
			if read.input.f != RegisterReadF || read.op.Type != history.OK {
				continue
			}
			before := int64(-1 << 63)
			if read.output.value != "" {
				source, ok := acknowledged[read.output.value]
				if !ok {
					continue
				}
				before = source.Complete
			}
			for _, l := range loads {
				if before >= l.op.Invoke {
					continue
				}
				for _, w := range l.witness {
					if w.key == key || w.complete >= read.op.Invoke {
						continue
					}
					found.add(RegisterLoadAtomic, func() map[string]any {
						details := map[string]any{"key": key, "read": describe(read.op), "load": describe(l.op), "shown_on": w.key}
						if read.output.value == "" {
							details["value"] = nil
						} else {
							details["value"] = read.output.value
						}
						return details
					})
					continue reads
				}
			}
		}
	}
}

func tokenOf(op *history.Op) string {
	var token string
	_ = json.Unmarshal(op.Input, &token)
	return token
}

func withoutReads(ops []registerOp) []registerOp {
	kept := make([]registerOp, 0, len(ops))
	for _, op := range ops {
		if op.input.f != RegisterReadF {
			kept = append(kept, op)
		}
	}
	return kept
}

func checkMonotonicReads(key string, ops []registerOp, found *collector) {
	byValue := map[string]*history.Op{}
	for _, op := range ops {
		if op.op.Type == history.OK && op.input.f != RegisterReadF {
			byValue[op.input.value] = op.op
		}
	}
	last := map[int]registerOp{}
	for _, read := range ops {
		if read.input.f != RegisterReadF {
			continue
		}
		previous, ok := last[read.op.Process]
		last[read.op.Process] = read
		if !ok || previous.output.value == "" || previous.output.value == read.output.value {
			continue
		}
		evidence := func() map[string]any {
			details := map[string]any{"key": key, "previous_read": describe(previous.op), "previous_value": previous.output.value, "read": describe(read.op)}
			if read.output.value == "" {
				details["value"] = nil
			} else {
				details["value"] = read.output.value
			}
			return details
		}
		if read.output.value == "" {
			found.add(RegisterMonotonic, evidence)
			continue
		}
		older, olderOK := byValue[read.output.value]
		newer, newerOK := byValue[previous.output.value]
		if olderOK && newerOK && older.Complete < newer.Invoke {
			found.add(RegisterMonotonic, evidence)
		}
	}
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
