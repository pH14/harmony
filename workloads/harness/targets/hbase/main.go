// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"os"
	"strings"
	"time"

	"github.com/go-zookeeper/zk"
	"github.com/tsuna/gohbase"
	"github.com/tsuna/gohbase/hrpc"
	"github.com/tsuna/gohbase/pb"
	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
)

const quorum = "10.77.0.1:2181,10.77.0.2:2181,10.77.0.3:2181"
const table = "harmony"

type statusCall struct {
	*hrpc.ClusterStatus
	ctx context.Context
}

func (s statusCall) Context() context.Context { return s.ctx }

type adapter struct{}

func (adapter) Family() family.Family { return family.Register{} }
func (adapter) Connect(context.Context, int) (runner.Client, error) {
	return &client{gohbase.NewClient(quorum)}, nil
}

type client struct{ h gohbase.Client }

func (c *client) Close() { c.h.Close() }
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	switch op.F {
	case family.RegisterReadF:
		req, err := hrpc.NewGetStr(ctx, table, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		result, err := c.h.Get(req)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if len(result.Cells) == 0 {
			return nil, nil
		}
		if len(result.Cells) != 1 {
			return nil, runner.Definite(fmt.Errorf("unexpected cell count %d", len(result.Cells)))
		}
		return string(result.Cells[0].Value), nil
	case family.RegisterWrite, family.RegisterCAS:
		value, expected := "", ""
		if op.F == family.RegisterCAS {
			pair := op.V.([]string)
			expected, value = pair[0], pair[1]
		} else {
			value = op.V.(string)
		}
		req, err := hrpc.NewPutStr(ctx, table, op.K, map[string]map[string][]byte{"f": {"v": []byte(value)}}, hrpc.Durability(hrpc.SyncWal))
		if err != nil {
			return nil, runner.Definite(err)
		}
		if op.F == family.RegisterCAS {
			ok, err := c.h.CheckAndPut(req, "f", "v", []byte(expected))
			if err != nil {
				return nil, err
			}
			if !ok {
				return nil, runner.Definite(fmt.Errorf("compare failed"))
			}
			return nil, nil
		}
		_, err = c.h.Put(req)
		return nil, err
	default:
		return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
	}
}
func (adapter) Ready(ctx context.Context) error {
	conn, events, err := zk.Connect(strings.Split(quorum, ","), 10*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	go func() {
		for range events {
		}
	}()
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		children, _, err := conn.Children("/hbase/rs")
		if err == nil && len(children) == 2 {
			break
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for two RegionServers: %w", ctx.Err())
		case <-ticker.C:
		}
	}
	admin := gohbase.NewAdminClient(quorum)
	err = admin.CreateTable(hrpc.NewCreateTable(ctx, []byte(table), map[string]map[string]string{"f": {"COMPRESSION": "NONE"}}, hrpc.SplitKeys([][]byte{[]byte("k2")})))
	if err != nil && !strings.Contains(err.Error(), "TableExistsException") {
		return err
	}
	c := &client{gohbase.NewClient(quorum)}
	defer c.Close()
	for _, key := range []string{"_ready", "z_ready"} {
		if _, err := c.Invoke(ctx, family.Op{F: family.RegisterWrite, K: key, V: "ready"}); err != nil {
			return err
		}
		value, err := c.Invoke(ctx, family.Op{F: family.RegisterReadF, K: key})
		if err != nil {
			return err
		}
		if value != "ready" {
			return fmt.Errorf("readiness value: %v", value)
		}
	}
	for {
		message, err := admin.(gohbase.RPCClient).SendRPC(statusCall{hrpc.NewClusterStatus(), ctx})
		if err != nil {
			return err
		}
		status, ok := message.(*pb.GetClusterStatusResponse)
		if !ok {
			return fmt.Errorf("unexpected cluster status %T", message)
		}
		owners := 0
		for _, server := range status.GetClusterStatus().GetLiveServers() {
			for _, region := range server.GetServerLoad().GetRegionLoads() {
				if bytes.HasPrefix(region.GetRegionSpecifier().GetValue(), []byte(table+",")) {
					owners++
					break
				}
			}
		}
		if owners == 2 {
			break
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for table regions on two servers: %w", ctx.Err())
		case <-ticker.C:
		}
	}
	fmt.Println("HBASE_READY two-regionservers two-regions sync-wal-write-read")
	return nil
}
func main() {
	if len(os.Args) == 2 && os.Args[1] == "wait-namenode" {
		ctx, cancel := context.WithTimeout(context.Background(), 500*time.Second)
		defer cancel()
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for {
			conn, err := (&net.Dialer{Timeout: time.Second}).DialContext(ctx, "tcp", "10.77.0.4:9000")
			if err == nil {
				conn.Close()
				return
			}
			select {
			case <-ctx.Done():
				fmt.Fprintln(os.Stderr, ctx.Err())
				os.Exit(1)
			case <-ticker.C:
			}
		}
	}
	runner.Main(adapter{})
}
