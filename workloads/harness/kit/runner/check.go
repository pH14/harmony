// SPDX-License-Identifier: AGPL-3.0-or-later

package runner

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/history"
	"harmony.local/harness/kit/sdk"
)

const (
	CheckReadSucceeded      = "harness: a check read succeeded"
	CheckReadAfterDisturbed = "harness: a check read succeeded after a fault"
	CheckNodeReadSucceeded  = "harness: a check read pinned to a node succeeded"
)

const generationEnv = "HARMONY_DISTURBANCE_GENERATION"

const faultsInForceEnv = "HARMONY_FAULTS_IN_FORCE"

const baselineFile = "disturbance-baseline"

const checkProcess = 0

type CheckResult struct {
	Read          history.Type            `json:"read"`
	Nodes         map[string]history.Type `json:"nodes,omitempty"`
	Ops           int                     `json:"ops"`
	Decided       bool                    `json:"decided"`
	UndecidedKeys int                     `json:"undecided_keys"`
	Violations    []family.Violation      `json:"violations"`
	Facts         map[string]bool         `json:"facts"`
	Replicas      *ReplicaResult          `json:"replicas,omitempty"`
}

type ReplicaResult struct {
	Rounds  int                     `json:"rounds,omitempty"`
	Agreed  bool                    `json:"agreed"`
	Nodes   map[string]history.Type `json:"nodes,omitempty"`
	Skipped string                  `json:"skipped,omitempty"`
}

func (r CheckResult) String() string {
	text, err := json.Marshal(r)
	if err != nil {
		return err.Error()
	}
	return string(text)
}

func Declare(fam family.Family) {
	for _, kind := range fam.Kinds() {
		sdk.Declare(sdk.KindAlways, kind)
	}
	for _, observation := range fam.Observations() {
		sdk.Declare(sdk.KindSometimes, observation)
	}
	sdk.Declare(sdk.KindSometimes, CheckReadSucceeded)
	sdk.Declare(sdk.KindSometimes, CheckReadAfterDisturbed)
}

func pinnedNodes(adapter Adapter) []string {
	if pinner, ok := adapter.(Pinner); ok {
		return pinner.Nodes()
	}
	return nil
}

func Check(ctx context.Context, adapter Adapter, config Config) (CheckResult, error) {
	fam := adapter.Family()
	Declare(fam)
	nodes := pinnedNodes(adapter)
	if len(nodes) > 0 {
		sdk.Declare(sdk.KindSometimes, CheckNodeReadSucceeded)
	}
	prior, err := history.Load(config.Dir)
	if err != nil {
		return CheckResult{}, err
	}
	journal, err := history.Create(config.Dir, "check", sdk.Random)
	if err != nil {
		return CheckResult{}, err
	}
	recorder := fam.Recorder(prior)
	outcomes := make([]history.Type, len(nodes)+1)
	errs := make([]error, len(nodes)+2)
	var wg sync.WaitGroup
	var replicas *ReplicaResult
	if replicated, ok := fam.(family.Replicated); ok {
		wg.Add(1)
		go func() {
			defer wg.Done()
			replicas, errs[len(nodes)+1] = compareReplicas(ctx, adapter, replicated, recorder, journal, nodes, config)
		}()
	}
	for index := range outcomes {
		op := fam.Read()
		if index > 0 {
			op.Node = nodes[index-1]
		}
		wg.Add(1)
		go func() {
			defer wg.Done()
			outcomes[index], errs[index] = checkRead(ctx, adapter, recorder, journal, checkProcess+index, op, config.CheckTimeout)
		}()
	}
	wg.Wait()
	closeErr := journal.Close()
	if err := errors.Join(errs...); err != nil {
		return CheckResult{}, err
	}
	if closeErr != nil {
		return CheckResult{}, closeErr
	}
	h, err := history.Load(config.Dir)
	if err != nil {
		return CheckResult{}, err
	}
	report := fam.Check(h)
	Emit(fam, report)
	readOK := outcomes[0] == history.OK
	sdk.Sometimes(readOK, CheckReadSucceeded, nil)
	if disturbed, known := disturbedSinceFirstCheck(config.Dir); known {
		sdk.Sometimes(readOK && disturbed, CheckReadAfterDisturbed, nil)
	}
	result := CheckResult{
		Read:          outcomes[0],
		Ops:           len(h.Ops),
		Decided:       report.Decided,
		UndecidedKeys: report.UndecidedKeys,
		Violations:    report.Violations,
		Facts:         report.Facts,
		Replicas:      replicas,
	}
	if len(nodes) > 0 {
		result.Nodes = map[string]history.Type{}
		nodeOK := false
		for index, node := range nodes {
			result.Nodes[node] = outcomes[index+1]
			nodeOK = nodeOK || outcomes[index+1] == history.OK
		}
		sdk.Sometimes(nodeOK, CheckNodeReadSucceeded, nil)
	}
	return result, nil
}

func checkRead(ctx context.Context, adapter Adapter, recorder family.Recorder, journal *history.Journal, p int, op family.Op, timeout time.Duration) (history.Type, error) {
	client, err := connect(ctx, adapter, p, timeout)
	if err != nil {
		return "", nil
	}
	defer client.Close()
	index, err := journal.Invoke(p, op.F, op.K, op.Node, op.V)
	if err != nil {
		return "", err
	}
	result, err := invoke(ctx, client, op, timeout)
	switch {
	case err == nil:
		return history.OK, recorder.Record(journal, p, op, index, result)
	case IsDefinite(err):
		return history.Fail, journal.Complete(p, history.Fail, op.F, op.K, index, errorValue(err))
	default:
		return history.Info, journal.Complete(p, history.Info, op.F, op.K, index, errorValue(err))
	}
}

func compareReplicas(ctx context.Context, adapter Adapter, replicated family.Replicated, recorder family.Recorder, journal *history.Journal, nodes []string, config Config) (*ReplicaResult, error) {
	if len(nodes) < 2 {
		return &ReplicaResult{Skipped: "the adapter pins reads to fewer than two nodes"}, nil
	}
	// Replicas need only agree once faults end. A member that a fault still
	// cuts off may answer from its own lagging copy, so no round runs and no
	// replica record reaches the journal that later checks reread.
	if os.Getenv(faultsInForceEnv) == "1" {
		return &ReplicaResult{Skipped: "a fault is in force"}, nil
	}
	rounds, settle := replicated.Settle()
	result := &ReplicaResult{}
	for round := range rounds {
		if round > 0 {
			timer := time.NewTimer(settle)
			select {
			case <-ctx.Done():
			case <-timer.C:
			}
			timer.Stop()
		}
		outcomes := make([]history.Type, len(nodes))
		errs := make([]error, len(nodes))
		var wg sync.WaitGroup
		for index, node := range nodes {
			op := replicated.Replicas(round)
			op.Node = node
			wg.Add(1)
			go func() {
				defer wg.Done()
				outcomes[index], errs[index] = checkRead(ctx, adapter, recorder, journal, len(nodes)+1+index, op, config.CheckTimeout)
			}()
		}
		wg.Wait()
		if err := errors.Join(errs...); err != nil {
			return nil, err
		}
		result.Rounds = round + 1
		result.Nodes = map[string]history.Type{}
		answered := 0
		for index, node := range nodes {
			result.Nodes[node] = outcomes[index]
			if outcomes[index] == history.OK {
				answered++
			}
		}
		if answered < 2 {
			break
		}
		h, err := history.Load(config.Dir)
		if err != nil {
			return nil, err
		}
		if !replicated.Disagree(h, journal.Name(), round) {
			result.Agreed = true
			break
		}
	}
	return result, nil
}

func Emit(fam family.Family, report family.Report) {
	byKind := map[string][]family.Violation{}
	for _, violation := range report.Violations {
		byKind[violation.Kind] = append(byKind[violation.Kind], violation)
	}
	for _, kind := range fam.Kinds() {
		violations := byKind[kind]
		for _, violation := range violations {
			sdk.Always(false, kind, violation.Details)
		}
		if len(violations) == 0 && report.Decided && !report.Undecided[kind] {
			sdk.Always(true, kind, nil)
		}
	}
	for _, observation := range fam.Observations() {
		sdk.Sometimes(report.Facts[observation], observation, nil)
	}
	sdk.CheckUndecided(report.UndecidedKeys)
}

func disturbedSinceFirstCheck(dir string) (bool, bool) {
	current, err := strconv.ParseUint(os.Getenv(generationEnv), 10, 64)
	if err != nil {
		return false, false
	}
	path := filepath.Join(dir, baselineFile)
	file, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644)
	if err == nil {
		_, writeErr := fmt.Fprintf(file, "%d\n", current)
		closeErr := file.Close()
		return false, writeErr == nil && closeErr == nil
	}
	if !errors.Is(err, os.ErrExist) {
		return false, false
	}
	text, err := os.ReadFile(path)
	if err != nil {
		return false, false
	}
	baseline, err := strconv.ParseUint(strings.TrimSpace(string(text)), 10, 64)
	if err != nil {
		return false, false
	}
	return current > baseline, true
}
