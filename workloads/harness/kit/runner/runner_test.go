// SPDX-License-Identifier: AGPL-3.0-or-later

package runner

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/history"
)

type memoryLog struct {
	mu     sync.Mutex
	values []string
	forget int
}

func (m *memoryLog) Family() family.Family { return family.Log{} }

func (m *memoryLog) Connect(context.Context, int) (Client, error) { return m, nil }

func (m *memoryLog) Close() {}

func (m *memoryLog) Invoke(_ context.Context, op family.Op) (any, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	switch op.F {
	case family.LogAppend:
		m.values = append(m.values, op.V.(string))
		position := uint64(len(m.values))
		if m.forget > 0 && len(m.values)%m.forget == 0 {
			m.values = m.values[:len(m.values)-1]
		}
		return position, nil
	case family.LogRead:
		return append([]string(nil), m.values...), nil
	}
	return nil, Definite(errors.New("unknown op"))
}

type memoryRegister struct {
	mu     sync.Mutex
	values map[string]string
}

func (m *memoryRegister) Family() family.Family { return family.Register{} }

func (m *memoryRegister) Connect(context.Context, int) (Client, error) { return m, nil }

func (m *memoryRegister) Close() {}

func (m *memoryRegister) Invoke(_ context.Context, op family.Op) (any, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	switch op.F {
	case family.RegisterWrite:
		m.values[op.K] = op.V.(string)
		return nil, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		if m.values[op.K] != pair[0] {
			return nil, Definite(errors.New("compare failed"))
		}
		m.values[op.K] = pair[1]
		return nil, nil
	case family.RegisterReadF:
		if value, ok := m.values[op.K]; ok {
			return value, nil
		}
		return nil, nil
	}
	return nil, Definite(errors.New("unknown op"))
}

func testConfig(t *testing.T) Config {
	t.Setenv("HARNESS_SEED", "7")
	t.Setenv("ANTITHESIS_OUTPUT_DIR", t.TempDir())
	t.Setenv(generationEnv, "3")
	config := Defaults()
	config.Dir = t.TempDir()
	config.Ops = 200
	config.MaxPause = 0
	config.Timeout = time.Second
	return config
}

type sdkRecord struct {
	Assert struct {
		Hit       bool   `json:"hit"`
		Type      string `json:"assert_type"`
		ID        string `json:"id"`
		Condition bool   `json:"condition"`
	} `json:"antithesis_assert"`
}

func failing(t *testing.T) map[string]bool {
	t.Helper()
	file, err := os.Open(filepath.Join(os.Getenv("ANTITHESIS_OUTPUT_DIR"), "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	failed := map[string]bool{}
	scanner := bufio.NewScanner(file)
	scanner.Buffer(nil, 1<<20)
	for scanner.Scan() {
		var record sdkRecord
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			t.Fatal(err)
		}
		if record.Assert.Hit && record.Assert.Type == "always" && !record.Assert.Condition {
			failed[record.Assert.ID] = true
		}
	}
	return failed
}

func TestCorrectLogIsClean(t *testing.T) {
	config := testConfig(t)
	adapter := &memoryLog{}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Violations) != 0 || !result.Decided || result.Read != "ok" {
		t.Fatalf("result %s", result)
	}
	if !result.Facts[family.LogAppendAcked] || result.Ops != 4*200+1 {
		t.Fatalf("result %s", result)
	}
	if failed := failing(t); len(failed) != 0 {
		t.Fatalf("failing assertions %v", failed)
	}
}

func TestLosingLogIsCaught(t *testing.T) {
	config := testConfig(t)
	adapter := &memoryLog{forget: 17}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	failed := failing(t)
	if !failed[family.LogLost] || !failed[family.LogPosition] || len(failed) != 2 {
		t.Fatalf("failing %v; result %s", failed, result)
	}
}

func TestValuesCarryTheJournalToken(t *testing.T) {
	config := testConfig(t)
	config.Ops = 20
	adapter := &memoryLog{}
	for range 2 {
		if err := Run(context.Background(), adapter, config); err != nil {
			t.Fatal(err)
		}
	}
	h, err := history.Load(config.Dir)
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, op := range h.Ops {
		if op.F != family.LogAppend {
			continue
		}
		var value string
		if err := json.Unmarshal(op.Input, &value); err != nil {
			t.Fatal(err)
		}
		token := strings.TrimSuffix(strings.TrimPrefix(op.Journal, "drive-"), history.Suffix)
		if !strings.HasPrefix(value, token+"-") || seen[value] {
			t.Fatalf("value %q in journal %s", value, op.Journal)
		}
		seen[value] = true
	}
	if len(seen) == 0 {
		t.Fatal("no appends")
	}
}

func TestCorrectRegisterIsClean(t *testing.T) {
	config := testConfig(t)
	adapter := &memoryRegister{values: map[string]string{}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Violations) != 0 || !result.Decided {
		t.Fatalf("result %s", result)
	}
}

type replicatedLog struct {
	memoryLog
	behind string
}

func (r *replicatedLog) Nodes() []string { return []string{"n1", "n2", "n3"} }

func (r *replicatedLog) Connect(context.Context, int) (Client, error) { return r, nil }

func (r *replicatedLog) Invoke(ctx context.Context, op family.Op) (any, error) {
	values, err := r.memoryLog.Invoke(ctx, op)
	if err != nil || op.F != family.LogRead || op.Node == "" || op.Node != r.behind {
		return values, err
	}
	var kept []string
	for index, value := range values.([]string) {
		if index%5 != 2 {
			kept = append(kept, value)
		}
	}
	return kept, nil
}

func TestCheckReadsEveryNode(t *testing.T) {
	config := testConfig(t)
	adapter := &replicatedLog{}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Violations) != 0 || result.Ops != 4*200+4 || len(result.Nodes) != 3 {
		t.Fatalf("result %s", result)
	}
	for node, outcome := range result.Nodes {
		if outcome != history.OK {
			t.Fatalf("node %s read %s", node, outcome)
		}
	}
	if failed := failing(t); len(failed) != 0 {
		t.Fatalf("failing %v", failed)
	}
	if !passed(t)[CheckNodeReadSucceeded] {
		t.Fatal("no pinned read succeeded")
	}
}

func TestNodeThatSkipsValuesIsNamed(t *testing.T) {
	config := testConfig(t)
	adapter := &replicatedLog{behind: "n2"}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	failed := failing(t)
	if !failed[family.LogNodeLost] || len(failed) != 1 {
		t.Fatalf("failing %v; result %s", failed, result)
	}
	for _, violation := range result.Violations {
		if violation.Details["node"] != "n2" {
			t.Fatalf("violation %+v", violation)
		}
	}
}

func passed(t *testing.T) map[string]bool {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(os.Getenv("ANTITHESIS_OUTPUT_DIR"), "sdk.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	held := map[string]bool{}
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		var record sdkRecord
		if err := json.Unmarshal([]byte(line), &record); err != nil {
			t.Fatal(err)
		}
		if record.Assert.Hit && record.Assert.Condition {
			held[record.Assert.ID] = true
		}
	}
	return held
}

type hanging struct{ memoryLog }

func (h *hanging) Connect(context.Context, int) (Client, error) { return h, nil }

func (h *hanging) Invoke(context.Context, family.Op) (any, error) {
	select {}
}

func TestUnreachableSystemIsSilent(t *testing.T) {
	config := testConfig(t)
	config.Ops = 2
	config.Timeout = 10 * time.Millisecond
	config.CheckTimeout = 10 * time.Millisecond
	adapter := &hanging{}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if result.Decided || len(result.Violations) != 0 || result.Read != "info" {
		t.Fatalf("result %s", result)
	}
	if failed := failing(t); len(failed) != 0 {
		t.Fatalf("failing %v", failed)
	}
}

func TestDisturbanceBaseline(t *testing.T) {
	dir := t.TempDir()
	t.Setenv(generationEnv, "3")
	if disturbed, known := disturbedSinceFirstCheck(dir); disturbed || !known {
		t.Fatal("the first check sets the baseline")
	}
	if disturbed, _ := disturbedSinceFirstCheck(dir); disturbed {
		t.Fatal("no disturbance yet")
	}
	t.Setenv(generationEnv, "5")
	if disturbed, known := disturbedSinceFirstCheck(dir); !disturbed || !known {
		t.Fatal("a later generation is a disturbance")
	}
	t.Setenv(generationEnv, "")
	if _, known := disturbedSinceFirstCheck(dir); known {
		t.Fatal("no generation means unknown")
	}
}
