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
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/history"
	"harmony.local/harness/kit/sdk"
)

const (
	CheckReadSucceeded      = "harness: a check read succeeded"
	CheckReadAfterDisturbed = "harness: a check read succeeded after a fault"
)

const generationEnv = "HARMONY_DISTURBANCE_GENERATION"

const baselineFile = "disturbance-baseline"

const checkProcess = 0

type CheckResult struct {
	Read       history.Type       `json:"read"`
	Ops        int                `json:"ops"`
	Decided    bool               `json:"decided"`
	Violations []family.Violation `json:"violations"`
	Facts      map[string]bool    `json:"facts"`
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

func Check(ctx context.Context, adapter Adapter, config Config) (CheckResult, error) {
	fam := adapter.Family()
	Declare(fam)
	prior, err := history.Load(config.Dir)
	if err != nil {
		return CheckResult{}, err
	}
	journal, err := history.Create(config.Dir, "check", sdk.Random)
	if err != nil {
		return CheckResult{}, err
	}
	outcome, err := checkRead(ctx, adapter, fam, fam.Recorder(prior), journal, config.CheckTimeout)
	closeErr := journal.Close()
	if err != nil {
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
	readOK := outcome == history.OK
	sdk.Sometimes(readOK, CheckReadSucceeded, nil)
	if disturbed, known := disturbedSinceFirstCheck(config.Dir); known {
		sdk.Sometimes(readOK && disturbed, CheckReadAfterDisturbed, nil)
	}
	return CheckResult{
		Read:       outcome,
		Ops:        len(h.Ops),
		Decided:    report.Decided,
		Violations: report.Violations,
		Facts:      report.Facts,
	}, nil
}

func checkRead(ctx context.Context, adapter Adapter, fam family.Family, recorder family.Recorder, journal *history.Journal, timeout time.Duration) (history.Type, error) {
	client, err := connect(ctx, adapter, checkProcess, timeout)
	if err != nil {
		return "", nil
	}
	defer client.Close()
	op := fam.Read()
	index, err := journal.Invoke(checkProcess, op.F, op.K, op.V)
	if err != nil {
		return "", err
	}
	result, err := invoke(ctx, client, op, timeout)
	switch {
	case err == nil:
		return history.OK, recorder.Record(journal, checkProcess, op, index, result)
	case IsDefinite(err):
		return history.Fail, journal.Complete(checkProcess, history.Fail, op.F, op.K, index, errorValue(err))
	default:
		return history.Info, journal.Complete(checkProcess, history.Info, op.F, op.K, index, errorValue(err))
	}
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
