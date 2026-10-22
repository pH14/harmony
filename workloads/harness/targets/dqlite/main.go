// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"database/sql/driver"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"time"

	"github.com/canonical/go-dqlite/client"
	dqlite "github.com/canonical/go-dqlite/driver"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	formedPath = "/tmp/dqlite/formed"
	database   = "harness.db"
	attempt    = time.Second
	peerWait   = 500 * time.Millisecond
)

var nodes = []string{"10.77.0.1:9001", "10.77.0.2:9001", "10.77.0.3:9001"}

type adapter struct{}

func (adapter) Family() family.Family { return family.Log{} }

func (adapter) Connect(context.Context, int) (runner.Client, error) { return &session{}, nil }

func quiet(client.LogLevel, string, ...any) {}

// A connection asks the nodes for the leader in a rotation that starts at a
// node drawn from the SDK stream, and opens the database on the leader.
// dqlite serves reads and writes only there. The connector dials the leader
// last, so the last dialed address names the node behind the connection.
func connect(ctx context.Context) (*dqlite.Conn, string, error) {
	start := int(sdk.Below(uint64(len(nodes))))
	infos := make([]client.NodeInfo, 0, len(nodes))
	for at := range nodes {
		index := (start + at) % len(nodes)
		infos = append(infos, client.NodeInfo{ID: uint64(index + 1), Address: nodes[index], Role: client.Voter})
	}
	store := client.NewInmemNodeStore()
	if err := store.Set(ctx, infos); err != nil {
		return nil, "", err
	}
	var dialed string
	dial := func(ctx context.Context, address string) (net.Conn, error) {
		dialed = address
		return client.DefaultDialFunc(ctx, address)
	}
	drv, err := dqlite.New(store, dqlite.WithLogFunc(quiet), dqlite.WithAttemptTimeout(attempt), dqlite.WithDialFunc(dial))
	if err != nil {
		return nil, "", err
	}
	connector, err := drv.OpenConnector(database)
	if err != nil {
		return nil, "", err
	}
	conn, err := connector.Connect(ctx)
	if err != nil {
		return nil, "", err
	}
	return conn.(*dqlite.Conn), dialed, nil
}

type session struct {
	conn   *dqlite.Conn
	leader string
}

func (s *session) Close() {
	if s.conn != nil {
		s.conn.Close()
		s.conn = nil
	}
}

// The driver reports a lost connection, a lost leadership and a deposed
// leader with the same error, so every failure after the connection opens
// leaves an append's outcome unknown.
func (s *session) Invoke(ctx context.Context, op family.Op) (any, error) {
	if s.conn == nil {
		conn, leader, err := connect(ctx)
		if err != nil {
			return nil, runner.Definite(err)
		}
		s.conn, s.leader = conn, leader
	}
	out, err := s.run(ctx, op)
	if err != nil {
		s.Close()
	}
	return out, err
}

func (s *session) run(ctx context.Context, op family.Op) (any, error) {
	switch op.F {
	case family.LogAppend:
		result, err := s.conn.ExecContext(ctx, "INSERT INTO log (value) VALUES (?)",
			[]driver.NamedValue{{Ordinal: 1, Value: op.V.(string)}})
		if err != nil {
			return nil, err
		}
		id, err := result.LastInsertId()
		if err != nil {
			return nil, err
		}
		return uint64(id), nil
	case family.LogRead:
		values, err := read(ctx, s.conn)
		if err == nil {
			err = confirm(ctx, s.leader)
		}
		if err != nil {
			return nil, runner.Definite(err)
		}
		return values, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func read(ctx context.Context, conn *dqlite.Conn) ([]string, error) {
	rows, err := conn.QueryContext(ctx, "SELECT value FROM log ORDER BY pos", nil)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	values := []string{}
	row := make([]driver.Value, 1)
	for {
		err := rows.Next(row)
		// The driver wraps a connection that closes while it fetches the next
		// batch of rows around io.EOF, so only the bare value ends the rows.
		if err == io.EOF {
			return values, nil
		}
		if err != nil {
			return nil, err
		}
		switch value := row[0].(type) {
		case string:
			values = append(values, value)
		case []byte:
			values = append(values, string(value))
		default:
			return nil, fmt.Errorf("log value has type %T", row[0])
		}
	}
}

// dqlite answers a query from the leader's own database without a Raft round
// trip, so a deposed leader that has not yet stepped down answers from stale
// state. A read counts only when another node names the reading node as its
// leader after the query returns. That node has then accepted no entry from a
// later leader, and in three nodes a later leader commits nothing without it
// or the reading node.
func confirm(ctx context.Context, leader string) error {
	var errs []error
	for _, peer := range nodes {
		if peer == leader {
			continue
		}
		named, err := leaderOf(ctx, peer)
		if err == nil && named == leader {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s names leader %q: %w", peer, named, err))
	}
	return fmt.Errorf("leadership of %s not confirmed: %w", leader, errors.Join(errs...))
}

func leaderOf(ctx context.Context, peer string) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, peerWait)
	defer cancel()
	cli, err := client.New(ctx, peer, client.WithLogFunc(quiet))
	if err != nil {
		return "", err
	}
	defer cli.Close()
	info, err := cli.Leader(ctx)
	if err != nil {
		return "", err
	}
	if info == nil {
		return "", nil
	}
	return info.Address, nil
}

// The first probe adds nodes 2 and 3 as voters to the cluster that node 1
// bootstraps and creates the tables. Every probe then needs one committed
// write.
func (adapter) Ready(ctx context.Context) error {
	if _, err := os.Stat(formedPath); err != nil {
		if err := form(ctx); err != nil {
			return err
		}
		if err := os.WriteFile(formedPath, nil, 0o644); err != nil {
			return err
		}
	}
	conn, _, err := connect(ctx)
	if err != nil {
		return err
	}
	defer conn.Close()
	_, err = conn.ExecContext(ctx, "UPDATE ready SET n = n + 1", nil)
	return err
}

func form(ctx context.Context) error {
	cli, err := client.New(ctx, nodes[0], client.WithLogFunc(quiet))
	if err != nil {
		return err
	}
	defer cli.Close()
	members, err := cli.Cluster(ctx)
	if err != nil {
		return fmt.Errorf("cluster: %w", err)
	}
	roles := map[uint64]client.NodeRole{}
	for _, member := range members {
		roles[member.ID] = member.Role
	}
	for at, addr := range nodes[1:] {
		id := uint64(at + 2)
		role, present := roles[id]
		switch {
		case !present:
			err = cli.Add(ctx, client.NodeInfo{ID: id, Address: addr, Role: client.Voter})
		case role != client.Voter:
			err = cli.Assign(ctx, id, client.Voter)
		}
		if err != nil {
			return fmt.Errorf("add node %d: %w", id, err)
		}
	}
	conn, _, err := connect(ctx)
	if err != nil {
		return err
	}
	defer conn.Close()
	for _, statement := range []string{
		"CREATE TABLE IF NOT EXISTS log (pos INTEGER PRIMARY KEY AUTOINCREMENT, value TEXT NOT NULL)",
		"CREATE TABLE IF NOT EXISTS ready (n INTEGER NOT NULL)",
		"INSERT INTO ready (n) SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM ready)",
	} {
		if _, err := conn.ExecContext(ctx, statement, nil); err != nil {
			return fmt.Errorf("%s: %w", statement, err)
		}
	}
	return nil
}

func main() { runner.Main(adapter{}) }
