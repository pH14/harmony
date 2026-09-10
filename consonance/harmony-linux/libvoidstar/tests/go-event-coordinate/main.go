// SPDX-License-Identifier: AGPL-3.0-or-later

// go-event-coordinate is a small concurrent fixture for the instrumented-event
// crash coordinate. The host releases work only after libvoidstar acknowledges
// an arm, and channel rendezvous make the application state order explicit.
package main

import (
	"fmt"
	"os"
	"strconv"
	"sync"
)

const (
	workerCount  = 2
	roundCount   = 16
	expectedSum  = 344
	startFDEnv   = "HARMONY_FIXTURE_START_FD"
	successToken = "GO_EVENT_COORDINATE_DONE rounds=16 sum=344"
)

type result struct {
	worker int
	round  int
	value  int
}

func main() {
	start := inheritedFile(startFDEnv)
	defer start.Close()

	inputs := [workerCount]chan int{make(chan int), make(chan int)}
	results := make(chan result)
	ready := make(chan int, workerCount)
	var workers sync.WaitGroup
	workers.Add(workerCount)
	for worker := range workerCount {
		go runWorker(worker, inputs[worker], results, ready, &workers)
	}
	for range workerCount {
		<-ready
	}
	marker("GO_EVENT_COORDINATE_READY workers=2")

	var release [1]byte
	if count, err := start.Read(release[:]); err != nil || count != len(release) {
		fail("start-channel")
	}
	marker("GO_EVENT_COORDINATE_RELEASED")

	sum := 0
	for round := range roundCount {
		worker := round % workerCount
		marker(fmt.Sprintf(
			"GO_EVENT_COORDINATE_STATE round=%d worker=%d sum=%d", round, worker, sum))
		inputs[worker] <- round
		item := <-results
		if item.worker != worker || item.round != round {
			fail("result-order")
		}
		sum += item.value
	}
	for worker := range workerCount {
		close(inputs[worker])
	}
	workers.Wait()
	if sum != expectedSum {
		fail("sum")
	}
	marker(successToken)

	// A coordinate beyond the fixture's event range must fail at the harness
	// deadline rather than being mistaken for a successful crash.
	_, _ = start.Read(release[:])
}

func runWorker(
	worker int,
	input <-chan int,
	results chan<- result,
	ready chan<- int,
	workers *sync.WaitGroup,
) {
	defer workers.Done()
	ready <- worker
	for round := range input {
		results <- result{
			worker: worker,
			round:  round,
			value:  (round + 1) * (worker + 2),
		}
	}
}

func inheritedFile(name string) *os.File {
	text := os.Getenv(name)
	fd, err := strconv.Atoi(text)
	if err != nil || fd < 0 {
		fail("missing-" + name)
	}
	return os.NewFile(uintptr(fd), name)
}

func marker(text string) {
	fmt.Fprintln(os.Stdout, text)
}

func fail(reason string) {
	fmt.Fprintln(os.Stdout, "GO_EVENT_COORDINATE_FAIL reason="+reason)
	os.Exit(1)
}
