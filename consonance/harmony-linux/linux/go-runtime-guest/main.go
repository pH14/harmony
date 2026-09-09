// SPDX-License-Identifier: AGPL-3.0-or-later

// go-runtime-proof is a deliberately small Linux/amd64 guest workload. It
// uses the ordinary Go runtime directly: no cgo, SDK, or instrumentation.
// Keep the output below stable because the boot harness compares it across
// same-seed runs.
package main

import (
	"fmt"
	"os"
	"sync"
	"time"
)

const (
	workerCount = 3
	taskCount   = 12

	// These are long enough to exercise the runtime timer path while keeping
	// the probe small. Their elapsed durations are deliberately never printed.
	workerSleep = time.Millisecond
	timerDelay  = 2 * time.Millisecond

	// This is the checksum of taskChecksum(0)..taskChecksum(taskCount-1).
	// Keeping it as a literal means a changed payload or fold fails loudly.
	expectedChecksum uint64 = 0x2881f549424eb0d1
)

var payloads = [...]string{
	"harmony",
	"virtual-clock",
	"goroutine",
	"channel",
}

type result struct {
	task     int
	checksum uint64
}

func main() {
	marker("GO_RUNTIME_PROOF_BOOT")

	jobs := make(chan int)
	results := make(chan result, taskCount)
	ready := make(chan struct{})

	var workers sync.WaitGroup
	workers.Add(workerCount)
	for range workerCount {
		go func() {
			defer workers.Done()
			ready <- struct{}{}
			for task := range jobs {
				results <- result{task: task, checksum: taskChecksum(task)}
				sleptAt := time.Now()
				time.Sleep(workerSleep)
				if time.Since(sleptAt) < workerSleep {
					fail("sleep-early")
				}
			}
		}()
	}

	for range workerCount {
		<-ready
	}
	marker("GO_RUNTIME_PROOF_GOROUTINES workers=3")

	for task := 0; task < taskCount; task++ {
		jobs <- task
	}
	close(jobs)

	seen := make([]bool, taskCount)
	var checksum uint64
	for range taskCount {
		item := <-results
		if item.task < 0 || item.task >= taskCount {
			fail("invalid-task")
		}
		if seen[item.task] {
			fail("duplicate-task")
		}
		seen[item.task] = true
		checksum += item.checksum
	}
	for _, complete := range seen {
		if !complete {
			fail("missing-task")
		}
	}
	if checksum != expectedChecksum {
		fail("checksum")
	}
	marker("GO_RUNTIME_PROOF_CHANNELS count=12 checksum=" + fmt.Sprintf("%016x", checksum))

	workers.Wait()
	marker("GO_RUNTIME_PROOF_SLEEP done=1")

	// Start and consume the timer immediately before checking it. Measuring
	// here prevents the channel workload from hiding an early timer wakeup.
	timerStarted := time.Now()
	timer := time.NewTimer(timerDelay)
	<-timer.C
	if time.Since(timerStarted) < timerDelay {
		fail("timer-early")
	}
	marker("GO_RUNTIME_PROOF_TIMER fired=1")
	marker("GO_RUNTIME_OK count=12 checksum=" + fmt.Sprintf("%016x", checksum))

	// The harness stops at the success marker. Keep an armed long timer so PID1
	// remains alive without triggering the runtime's all-goroutines-asleep
	// deadlock panic; the harness can checkpoint or stop this process.
	for {
		time.Sleep(time.Hour)
	}
}

func taskChecksum(task int) uint64 {
	const (
		offset = uint64(14695981039346656037)
		prime  = uint64(1099511628211)
	)

	hash := offset
	hash ^= uint64(task + 1)
	hash *= prime
	payload := payloads[task%len(payloads)]
	for i := 0; i < len(payload); i++ {
		hash ^= uint64(payload[i])
		hash *= prime
	}
	return hash
}

func marker(text string) {
	fmt.Fprintln(os.Stdout, text)
}

func fail(reason string) {
	fmt.Fprintln(os.Stdout, "GO_RUNTIME_FAIL reason="+reason)
	os.Exit(1)
}
