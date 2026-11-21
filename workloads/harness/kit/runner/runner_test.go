// SPDX-License-Identifier: AGPL-3.0-or-later

package runner

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strconv"
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

type replicatedRegister struct {
	memoryRegister
	versions map[string]uint64
	behind   string
}

func (r *replicatedRegister) Family() family.Family {
	return family.Register{Pause: time.Millisecond}
}

func (r *replicatedRegister) Nodes() []string { return []string{"n1", "n2", "n3"} }

func (r *replicatedRegister) Connect(context.Context, int) (Client, error) { return r, nil }

func (r *replicatedRegister) Invoke(ctx context.Context, op family.Op) (any, error) {
	if op.F != family.RegisterReplicas {
		result, err := r.memoryRegister.Invoke(ctx, op)
		if err == nil && op.F != family.RegisterReadF {
			r.mu.Lock()
			r.versions[op.K]++
			r.mu.Unlock()
		}
		return result, err
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	copies := map[string]family.Copy{}
	for _, key := range op.V.(family.ReplicaQuery).Keys {
		value, ok := r.values[key]
		if !ok {
			copies[key] = family.Copy{}
			continue
		}
		version := r.versions[key]
		if op.Node == r.behind {
			version--
		}
		copies[key] = family.Copy{Value: &value, Meta: map[string]uint64{"version": version}}
	}
	return copies, nil
}

func TestReplicasThatAgreeTakeOneRound(t *testing.T) {
	config := testConfig(t)
	adapter := &replicatedRegister{memoryRegister: memoryRegister{values: map[string]string{}}, versions: map[string]uint64{}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Violations) != 0 || result.Replicas == nil || result.Replicas.Rounds != 1 || !result.Replicas.Agreed {
		t.Fatalf("result %s", result)
	}
	if !passed(t)[family.RegisterReplicasAgree] {
		t.Fatal("the replica property did not pass")
	}
}

func TestReplicaBehindOnMetadataIsCaught(t *testing.T) {
	config := testConfig(t)
	adapter := &replicatedRegister{memoryRegister: memoryRegister{values: map[string]string{}}, versions: map[string]uint64{}, behind: "n3"}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	failed := failing(t)
	if !failed[family.RegisterReplicasAgree] || len(failed) != 1 || result.Replicas.Rounds != 3 || result.Replicas.Agreed {
		t.Fatalf("failing %v; result %s", failed, result)
	}
}

func TestReplicasAreComparedOnlyAfterFaultsEnd(t *testing.T) {
	config := testConfig(t)
	adapter := &replicatedRegister{memoryRegister: memoryRegister{values: map[string]string{}}, versions: map[string]uint64{}, behind: "n3"}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	t.Setenv(faultsInForceEnv, "1")
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if failed := failing(t); len(failed) != 0 || result.Replicas == nil || result.Replicas.Skipped == "" || result.Replicas.Rounds != 0 {
		t.Fatalf("failing %v; result %s", failed, result)
	}
	t.Setenv(faultsInForceEnv, "0")
	result, err = Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if failed := failing(t); !failed[family.RegisterReplicasAgree] || result.Replicas.Rounds != 3 {
		t.Fatalf("failing %v; result %s", failed, result)
	}
}

func TestReplicasNeedTwoNodes(t *testing.T) {
	config := testConfig(t)
	adapter := &memoryRegister{values: map[string]string{}}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if result.Replicas == nil || result.Replicas.Skipped == "" || result.Replicas.Rounds != 0 {
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

type laggingNode struct {
	replicatedLog
	node     string
	failures int
	attempts int
}

func (l *laggingNode) Connect(context.Context, int) (Client, error) { return l, nil }

func (l *laggingNode) Invoke(ctx context.Context, op family.Op) (any, error) {
	if op.Node == l.node {
		l.mu.Lock()
		l.attempts++
		failing := l.failures != 0
		if l.failures > 0 {
			l.failures--
		}
		l.mu.Unlock()
		if failing {
			return nil, errors.New("index not applied")
		}
	}
	return l.replicatedLog.Invoke(ctx, op)
}

func recoveryConfig(t *testing.T, recovery time.Duration) (Config, string) {
	config := testConfig(t)
	config.CheckTimeout = 50 * time.Millisecond
	path := filepath.Join(t.TempDir(), "nodes")
	t.Setenv(faultsInForceEnv, "0")
	t.Setenv(quietEnv, "0")
	t.Setenv(recoveryEnv, strconv.FormatInt(int64(recovery), 10))
	t.Setenv(checkNodesEnv, path)
	return config, path
}

func TestPinnedReadsRetryWhileANodeCatchesUp(t *testing.T) {
	config, path := recoveryConfig(t, 5*time.Second)
	adapter := &progressingNode{laggingNode: laggingNode{node: "n2", failures: 2}, fence: 20, applied: []uint64{20}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if result.Nodes["n2"] != history.OK || len(result.Unrecovered) != 0 || adapter.attempts != 3 {
		t.Fatalf("attempts %d; result %s", adapter.attempts, result)
	}
	if text, err := os.ReadFile(path); err != nil || len(text) != 0 {
		t.Fatalf("nodes file %q: %v", text, err)
	}
	if failed := failing(t); len(failed) != 0 {
		t.Fatalf("failing %v", failed)
	}
}

func TestANodeThatNeverAnswersIsListedAfterTheRecoveryBound(t *testing.T) {
	config, path := recoveryConfig(t, 1200*time.Millisecond)
	t.Setenv(quietEnv, strconv.FormatInt(int64(200*time.Millisecond), 10))
	adapter := &progressingNode{laggingNode: laggingNode{node: "n2", failures: -1}, fenceErr: errors.New("not leader"), applied: []uint64{7}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if elapsed := time.Since(start); elapsed < time.Second {
		t.Fatalf("gave up after %s", elapsed)
	}
	want := "progress fence: not leader; last pinned read: index not applied"
	if len(result.Unrecovered) != 1 || result.Unrecovered["n2"] != want || result.Nodes["n1"] != history.OK || result.Nodes["n3"] != history.OK {
		t.Fatalf("result %s", result)
	}
	text, err := os.ReadFile(path)
	if err != nil || string(text) != "n2\t"+want+"\n" {
		t.Fatalf("nodes file %q: %v", text, err)
	}
	if adapter.attempts < 2 {
		t.Fatalf("attempts %d", adapter.attempts)
	}
}

func TestPinnedReadsAreNotRetriedWhileAFaultIsInForce(t *testing.T) {
	config, path := recoveryConfig(t, 5*time.Second)
	t.Setenv(faultsInForceEnv, "1")
	adapter := &progressingNode{laggingNode: laggingNode{node: "n2", failures: -1}, fence: 20, applied: []uint64{7}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if adapter.attempts != 1 || result.Unrecovered != nil || result.Nodes["n2"] != history.Info {
		t.Fatalf("attempts %d; result %s", adapter.attempts, result)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("nodes file written: %v", err)
	}
}

func TestPinnedReadsAreNotRetriedWithoutARecoveryBound(t *testing.T) {
	config := testConfig(t)
	adapter := &progressingNode{laggingNode: laggingNode{node: "n2", failures: -1}, fence: 20, applied: []uint64{7}}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if adapter.attempts != 1 || result.Unrecovered != nil {
		t.Fatalf("attempts %d; result %s", adapter.attempts, result)
	}
}

func TestAnAdapterWithoutProgressReadsEachNodeOnceAndReportsNothing(t *testing.T) {
	config, path := recoveryConfig(t, 5*time.Second)
	adapter := &laggingNode{node: "n2", failures: -1}
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	if adapter.attempts != 1 || result.Unrecovered != nil || result.Nodes["n2"] != history.Info {
		t.Fatalf("attempts %d; result %s", adapter.attempts, result)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("nodes file written: %v", err)
	}
}

type progressingNode struct {
	laggingNode
	fence        uint64
	fenceErr     error
	applied      []uint64
	appliedErrs  []error
	appliedCalls int
	appliedAt    []time.Time
}

func (p *progressingNode) Fence(context.Context) (uint64, error) { return p.fence, p.fenceErr }

func (p *progressingNode) Applied(_ context.Context, node string) (uint64, error) {
	if node != p.node {
		return p.fence, nil
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	call := p.appliedCalls
	p.appliedCalls++
	p.appliedAt = append(p.appliedAt, time.Now())
	if call < len(p.appliedErrs) && p.appliedErrs[call] != nil {
		return 0, p.appliedErrs[call]
	}
	return p.applied[min(call, len(p.applied)-1)], nil
}

func checkProgress(t *testing.T, adapter *progressingNode) (CheckResult, string) {
	t.Helper()
	config, path := recoveryConfig(t, 300*time.Millisecond)
	saved := progressInterval
	progressInterval = 50 * time.Millisecond
	t.Cleanup(func() { progressInterval = saved })
	adapter.node, adapter.failures = "n2", -1
	if err := Run(context.Background(), adapter, config); err != nil {
		t.Fatal(err)
	}
	result, err := Check(context.Background(), adapter, config)
	if err != nil {
		t.Fatal(err)
	}
	text, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return result, string(text)
}

func TestANodeBehindTheFenceThatAppliesNothingIsUnrecovered(t *testing.T) {
	adapter := &progressingNode{fence: 20, applied: []uint64{7, 7}}
	result, text := checkProgress(t, adapter)
	want := "applied index 7, then applied index 7 50ms later, below progress fence 20; last pinned read: index not applied"
	if len(result.Unrecovered) != 1 || result.Unrecovered["n2"] != want {
		t.Fatalf("result %s", result)
	}
	if text != "n2\t"+want+"\n" {
		t.Fatalf("nodes file %q", text)
	}
	if len(adapter.appliedAt) != 2 || adapter.appliedAt[1].Sub(adapter.appliedAt[0]) < progressInterval {
		t.Fatalf("applied reads at %v", adapter.appliedAt)
	}
}

func TestANodeWithoutTwoAppliedIndexesIsNotReported(t *testing.T) {
	unanswered := errors.New("status timeout")
	for name, errs := range map[string][]error{
		"first read fails":  {unanswered},
		"second read fails": {nil, unanswered},
	} {
		t.Run(name, func(t *testing.T) {
			adapter := &progressingNode{fence: 20, applied: []uint64{7, 7}, appliedErrs: errs}
			result, text := checkProgress(t, adapter)
			if len(result.Unrecovered) != 0 || text != "" || adapter.appliedCalls != len(errs) {
				t.Fatalf("applied calls %d; nodes file %q; result %s", adapter.appliedCalls, text, result)
			}
		})
	}
}

func TestANodeBehindTheFenceThatAppliesMoreIsRecovering(t *testing.T) {
	adapter := &progressingNode{fence: 20, applied: []uint64{7, 9}}
	result, text := checkProgress(t, adapter)
	if len(result.Unrecovered) != 0 || text != "" || adapter.appliedCalls != 2 {
		t.Fatalf("applied calls %d; nodes file %q; result %s", adapter.appliedCalls, text, result)
	}
}

func TestANodeAtTheFenceIsReadOnce(t *testing.T) {
	adapter := &progressingNode{fence: 20, applied: []uint64{20}}
	result, text := checkProgress(t, adapter)
	if len(result.Unrecovered) != 0 || text != "" || adapter.appliedCalls != 1 {
		t.Fatalf("applied calls %d; nodes file %q; result %s", adapter.appliedCalls, text, result)
	}
}

func TestAFailedProgressFenceReportsEachUnansweredNode(t *testing.T) {
	adapter := &progressingNode{fenceErr: errors.New("not leader"), applied: []uint64{7}}
	result, text := checkProgress(t, adapter)
	want := "progress fence: not leader; last pinned read: index not applied"
	if len(result.Unrecovered) != 1 || result.Unrecovered["n2"] != want || text != "n2\t"+want+"\n" || adapter.appliedCalls != 0 {
		t.Fatalf("applied calls %d; nodes file %q; result %s", adapter.appliedCalls, text, result)
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

func TestAnyMemberPassesWhileAnotherMemberBlocks(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	start := time.Now()
	err := AnyMember(ctx, []string{"cut off", "healthy"}, func(ctx context.Context, member string) error {
		if member == "cut off" {
			<-ctx.Done()
			return ctx.Err()
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Fatalf("AnyMember waited %v for the blocked member", elapsed)
	}
}

func TestAnyMemberJoinsEveryFailure(t *testing.T) {
	first, second := errors.New("first"), errors.New("second")
	err := AnyMember(context.Background(), []error{first, second}, func(_ context.Context, member error) error {
		return member
	})
	if !errors.Is(err, first) || !errors.Is(err, second) {
		t.Fatalf("got %v", err)
	}
}
