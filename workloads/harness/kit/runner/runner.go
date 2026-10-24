// SPDX-License-Identifier: AGPL-3.0-or-later

package runner

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/history"
	"harmony.local/harness/kit/sdk"
)

type Client interface {
	Invoke(ctx context.Context, op family.Op) (any, error)
	Close()
}

type Adapter interface {
	Family() family.Family
	Connect(ctx context.Context, process int) (Client, error)
}

type Readier interface {
	Ready(ctx context.Context) error
}

type Pinner interface {
	Nodes() []string
}

type Config struct {
	Dir          string
	Processes    int
	Timeout      time.Duration
	CheckTimeout time.Duration
	ReadyTimeout time.Duration
	MaxPause     time.Duration
	Ops          int
}

func Defaults() Config {
	return Config{
		Dir:          "/tmp/harness/journal",
		Processes:    4,
		Timeout:      2 * time.Second,
		CheckTimeout: 10 * time.Second,
		ReadyTimeout: 10 * time.Second,
		MaxPause:     20 * time.Millisecond,
	}
}

type definite struct{ err error }

func (d definite) Error() string { return d.err.Error() }

func (d definite) Unwrap() error { return d.err }

func Definite(err error) error {
	if err == nil {
		return nil
	}
	return definite{err}
}

func IsDefinite(err error) bool {
	var d definite
	return errors.As(err, &d)
}

var errAbandoned = errors.New("adapter did not return after its deadline")

const abandonGrace = time.Second

func draw(n uint64) uint64 { return sdk.Below(n) }

func Run(ctx context.Context, adapter Adapter, config Config) error {
	fam := adapter.Family()
	journal, err := history.Create(config.Dir, "drive", sdk.Random)
	if err != nil {
		return err
	}
	defer journal.Close()
	recorder := fam.Recorder(nil)
	token := fmt.Sprintf("%016x", journal.Token())
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	errs := make(chan error, config.Processes)
	var wg sync.WaitGroup
	for p := 0; p < config.Processes; p++ {
		wg.Add(1)
		go func(p int) {
			defer wg.Done()
			if err := process(ctx, adapter, fam, recorder, journal, config, token, p); err != nil {
				errs <- err
				cancel()
			}
		}(p)
	}
	wg.Wait()
	close(errs)
	return <-errs
}

func process(ctx context.Context, adapter Adapter, fam family.Family, recorder family.Recorder, journal *history.Journal, config Config, token string, p int) error {
	generator := fam.Generator(p)
	counter := 0
	value := func() string {
		counter++
		return fmt.Sprintf("%s-%d-%d", token, p, counter)
	}
	var client Client
	defer func() {
		if client != nil {
			client.Close()
		}
	}()
	for done := 0; config.Ops == 0 || done < config.Ops; {
		if ctx.Err() != nil {
			return nil
		}
		if client == nil {
			connected, err := connect(ctx, adapter, p, config.Timeout)
			if err != nil {
				pause(ctx, config)
				continue
			}
			client = connected
		}
		op := generator.Next(draw, value)
		index, err := journal.Invoke(p, op.F, op.K, op.Node, op.V)
		if err != nil {
			return err
		}
		result, err := invoke(ctx, client, op, config.Timeout)
		switch {
		case err == nil:
			if err := recorder.Record(journal, p, op, index, result); err != nil {
				return err
			}
			generator.Observe(op, history.OK, result)
		case IsDefinite(err):
			if err := journal.Complete(p, history.Fail, op.F, op.K, index, errorValue(err)); err != nil {
				return err
			}
			generator.Observe(op, history.Fail, nil)
		default:
			if err := journal.Complete(p, history.Info, op.F, op.K, index, errorValue(err)); err != nil {
				return err
			}
			generator.Observe(op, history.Info, nil)
		}
		if errors.Is(err, errAbandoned) {
			go client.Close()
			client = nil
		}
		done++
		pause(ctx, config)
	}
	return nil
}

func errorValue(err error) map[string]string {
	return map[string]string{"error": err.Error()}
}

func connect(ctx context.Context, adapter Adapter, p int, timeout time.Duration) (Client, error) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	return adapter.Connect(ctx, p)
}

func invoke(ctx context.Context, client Client, op family.Op, timeout time.Duration) (any, error) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	type outcome struct {
		result any
		err    error
	}
	done := make(chan outcome, 1)
	go func() {
		result, err := client.Invoke(ctx, op)
		done <- outcome{result, err}
	}()
	grace := time.NewTimer(timeout + abandonGrace)
	defer grace.Stop()
	select {
	case out := <-done:
		return out.result, out.err
	case <-grace.C:
		return nil, errAbandoned
	}
}

func pause(ctx context.Context, config Config) {
	if config.MaxPause <= 0 {
		return
	}
	wait := time.Duration(draw(uint64(config.MaxPause/time.Millisecond)+1)) * time.Millisecond
	timer := time.NewTimer(wait)
	defer timer.Stop()
	select {
	case <-ctx.Done():
	case <-timer.C:
	}
}

func Main(adapter Adapter) {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: drive|check|ready [flags]")
		os.Exit(2)
	}
	config := Defaults()
	flags := flag.NewFlagSet(os.Args[1], flag.ExitOnError)
	flags.StringVar(&config.Dir, "dir", config.Dir, "journal directory")
	flags.IntVar(&config.Processes, "processes", config.Processes, "logical processes")
	flags.DurationVar(&config.Timeout, "timeout", config.Timeout, "operation timeout")
	flags.DurationVar(&config.CheckTimeout, "check-timeout", config.CheckTimeout, "check read timeout")
	flags.DurationVar(&config.ReadyTimeout, "ready-timeout", config.ReadyTimeout, "readiness timeout")
	flags.DurationVar(&config.MaxPause, "max-pause", config.MaxPause, "longest pause between operations")
	flags.IntVar(&config.Ops, "ops", config.Ops, "operations per process, 0 for no limit")
	_ = flags.Parse(os.Args[2:])
	ctx := context.Background()
	var err error
	switch os.Args[1] {
	case "drive":
		err = Run(ctx, adapter, config)
	case "check":
		var result CheckResult
		result, err = Check(ctx, adapter, config)
		if err == nil {
			fmt.Println(result.String())
		}
	case "ready":
		readier, ok := adapter.(Readier)
		if !ok {
			return
		}
		readyCtx, cancel := context.WithTimeout(ctx, config.ReadyTimeout)
		err = readier.Ready(readyCtx)
		cancel()
	default:
		err = fmt.Errorf("unknown subcommand %q", os.Args[1])
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
