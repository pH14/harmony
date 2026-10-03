// SPDX-License-Identifier: AGPL-3.0-or-later
package main

import (
	"bufio"
	"fmt"
	"os"
	"runtime"
	"sync/atomic"
	"time"
)

func main() {
	if len(os.Args) == 2 && os.Args[1] == "gc" {
		measureGC()
		return
	}
	var done atomic.Bool
	started := make(chan struct{})
	finished := make(chan struct{})
	go func() {
		close(started)
		for !done.Load() {
		}
		close(finished)
	}()
	<-started
	output := bufio.NewWriter(os.Stdout)
	fmt.Fprintln(output, "HARMONY_LANGUAGE_READY")
	if err := output.Flush(); err != nil {
		panic(err)
	}
	for marker := 1; marker <= 20; marker++ {
		time.Sleep(10 * time.Millisecond)
		fmt.Fprintf(output, "HARMONY_LANGUAGE_MARKER %02d\n", marker)
		if err := output.Flush(); err != nil {
			panic(err)
		}
	}
	done.Store(true)
	<-finished
}

func measureGC() {
	const count = 1 << 20
	objects := make([]*uint64, count)
	for i := range objects {
		objects[i] = new(uint64)
		*objects[i] = uint64(i)
	}
	for sample := 0; sample < 5; sample++ {
		var before, after runtime.MemStats
		runtime.ReadMemStats(&before)
		start := time.Now()
		runtime.GC()
		elapsed := time.Since(start)
		runtime.ReadMemStats(&after)
		fmt.Printf("HARMONY_GO_GC sample=%d heap_objects=%d elapsed_ns=%d stop_ns=%d\n",
			sample, after.HeapObjects, elapsed.Nanoseconds(), after.PauseTotalNs-before.PauseTotalNs)
	}
	runtime.KeepAlive(objects)
}
