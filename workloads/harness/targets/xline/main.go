// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	pb "go.etcd.io/etcd/api/v3/etcdserverpb"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

type adapter struct {
	members []string
	names   []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Nodes() []string { return a.names }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return client{members: a.members, names: a.names}, nil
}

type client struct {
	members []string
	names   []string
}

func (c client) Close() {}

// Each operation dials one member over a new connection, drawn from the SDK
// stream unless the check pins it, so requests reach followers as well as the
// leader.
func (c client) pick(node string) (string, error) {
	if node == "" {
		return c.members[sdk.Below(uint64(len(c.members)))], nil
	}
	at := slices.Index(c.names, node)
	if at < 0 {
		return "", fmt.Errorf("unknown node %q", node)
	}
	return c.members[at], nil
}

func (c client) Invoke(ctx context.Context, op family.Op) (any, error) {
	address, err := c.pick(op.Node)
	if err != nil {
		return nil, runner.Definite(err)
	}
	conn, err := grpc.NewClient(address, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, runner.Definite(err)
	}
	defer conn.Close()
	kv := pb.NewKVClient(conn)
	key := []byte(op.K)
	switch op.F {
	case family.RegisterWrite:
		_, err := kv.Put(ctx, &pb.PutRequest{Key: key, Value: []byte(op.V.(string))})
		return nil, classify(err)
	case family.RegisterCAS:
		pair := op.V.([]string)
		r, err := kv.Txn(ctx, &pb.TxnRequest{
			Compare: []*pb.Compare{{
				Key:         key,
				Result:      pb.Compare_EQUAL,
				Target:      pb.Compare_VALUE,
				TargetUnion: &pb.Compare_Value{Value: []byte(pair[0])},
			}},
			Success: []*pb.RequestOp{{Request: &pb.RequestOp_RequestPut{
				RequestPut: &pb.PutRequest{Key: key, Value: []byte(pair[1])},
			}}},
		})
		if err != nil {
			return nil, classify(err)
		}
		if !r.Succeeded {
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return nil, nil
	case family.RegisterReadF:
		r, err := kv.Range(ctx, &pb.RangeRequest{Key: key})
		if err != nil {
			return nil, runner.Definite(err)
		}
		switch len(r.Kvs) {
		case 0:
			return nil, nil
		case 1:
			return string(r.Kvs[0].Value), nil
		}
		return nil, runner.Definite(fmt.Errorf("range: %d values for one key", len(r.Kvs)))
	case family.RegisterReplicas:
		if err := fence(ctx, kv, op.Node); err != nil {
			return nil, runner.Definite(err)
		}
		copies, err := replicas(ctx, kv, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// A member applies committed entries in log order. Once a serializable range
// on the member returns the fence written after the comparison started, its
// state holds every write acknowledged before the comparison started. Each
// comparison writes its own fence key, so concurrent comparisons on one member
// cannot overwrite each other's fence.
func fence(ctx context.Context, kv pb.KVClient, node string) error {
	value := fmt.Sprintf("%016x", sdk.Random())
	key := []byte("fence-" + node + "-" + value)
	if _, err := kv.Put(ctx, &pb.PutRequest{Key: key, Value: []byte(value)}); err != nil {
		return fmt.Errorf("fence on %s: %w", node, err)
	}
	for {
		r, err := kv.Range(ctx, &pb.RangeRequest{Key: key, Serializable: true})
		if err == nil && len(r.Kvs) == 1 && string(r.Kvs[0].Value) == value {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("fence on %s: not applied: %w", node, ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

// A serializable range answers from the member's own applied state.
func replicas(ctx context.Context, kv pb.KVClient, keys []string) (map[string]family.Copy, error) {
	sorted := slices.Sorted(slices.Values(keys))
	r, err := kv.Range(ctx, &pb.RangeRequest{
		Key:          []byte(sorted[0]),
		RangeEnd:     []byte(sorted[len(sorted)-1] + "\x00"),
		Serializable: true,
	})
	if err != nil {
		return nil, err
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		copies[key] = family.Copy{}
	}
	for _, entry := range r.Kvs {
		if _, wanted := copies[string(entry.Key)]; !wanted {
			continue
		}
		text := string(entry.Value)
		copies[string(entry.Key)] = family.Copy{Value: &text, Meta: map[string]uint64{
			"create_revision": uint64(entry.CreateRevision),
			"mod_revision":    uint64(entry.ModRevision),
			"version":         uint64(entry.Version),
		}}
	}
	return copies, nil
}

// Only a failed dial is definite: any other error can follow a proposal that
// the cluster still commits.
func classify(err error) error {
	if err == nil {
		return nil
	}
	if s, ok := status.FromError(err); ok && s.Code() == codes.Unavailable &&
		strings.Contains(s.Message(), "Error while dialing") {
		return runner.Definite(err)
	}
	return err
}

func (a adapter) Ready(ctx context.Context) error {
	var errs []error
	for _, address := range a.members {
		conn, err := grpc.NewClient(address, grpc.WithTransportCredentials(insecure.NewCredentials()))
		if err != nil {
			errs = append(errs, err)
			continue
		}
		_, err = pb.NewKVClient(conn).Put(ctx, &pb.PutRequest{Key: []byte("ready"), Value: []byte("1")})
		conn.Close()
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s: %w", address, err))
	}
	return errors.Join(errs...)
}

func main() {
	members := make([]string, 3)
	names := make([]string, 3)
	for i := range members {
		members[i] = fmt.Sprintf("10.77.0.%d:2379", i+1)
		names[i] = fmt.Sprintf("n%d", i+1)
	}
	runner.Main(adapter{members: members, names: names})
}
