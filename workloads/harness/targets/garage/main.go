// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"time"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	bucket    = "harness"
	keyID     = "GK0123456789abcdef01234567"
	keySecret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
	region    = "garage"
)

type adapter struct {
	servers []*minio.Client
}

func (adapter) Family() family.Family { return family.Register{NoCAS: true} }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return &client{servers: a.servers}, nil
}

type client struct {
	servers []*minio.Client
}

func (c *client) Close() {}

func newServer(endpoint string) *minio.Client {
	transport := &http.Transport{
		Proxy:             nil,
		DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives: true,
	}
	s, err := minio.New(endpoint, &minio.Options{
		Creds:        credentials.NewStaticV4(keyID, keySecret, ""),
		Secure:       false,
		Region:       region,
		BucketLookup: minio.BucketLookupPath,
		Transport:    transport,
		MaxRetries:   1,
	})
	if err != nil {
		panic(err)
	}
	return s
}

// Every Garage node serves the S3 API and coordinates the quorum for the
// request, so each operation goes to one node drawn from the SDK stream.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	s := c.servers[sdk.Below(uint64(len(c.servers)))]
	switch op.F {
	case family.RegisterWrite:
		value := []byte(op.V.(string))
		_, err := s.PutObject(ctx, bucket, op.K, bytes.NewReader(value), int64(len(value)), minio.PutObjectOptions{DisableMultipart: true})
		return nil, classify(err)
	case family.RegisterReadF:
		value, err := get(ctx, s, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if value == nil {
			return nil, nil
		}
		return *value, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func get(ctx context.Context, s *minio.Client, key string) (*string, error) {
	object, err := s.GetObject(ctx, bucket, key, minio.GetObjectOptions{})
	if err != nil {
		return nil, err
	}
	defer object.Close()
	data, err := io.ReadAll(object)
	if err != nil {
		if minio.ToErrorResponse(err).Code == minio.NoSuchKey {
			return nil, nil
		}
		return nil, err
	}
	text := string(data)
	return &text, nil
}

// Only a failed dial is definite. Garage answers an error after a write that
// reached fewer nodes than the quorum, and those nodes keep it.
func classify(err error) error {
	if err == nil {
		return nil
	}
	var opErr *net.OpError
	if errors.As(err, &opErr) && opErr.Op == "dial" {
		return runner.Definite(err)
	}
	return err
}

func (a adapter) Ready(ctx context.Context) error {
	var errs []error
	for i, s := range a.servers {
		value := []byte("1")
		_, err := s.PutObject(ctx, bucket, "ready", bytes.NewReader(value), int64(len(value)), minio.PutObjectOptions{DisableMultipart: true})
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("n%d: %w", i+1, err))
	}
	return errors.Join(errs...)
}

func main() {
	servers := make([]*minio.Client, 3)
	for i := range servers {
		servers[i] = newServer(fmt.Sprintf("10.77.0.%d:3900", i+1))
	}
	runner.Main(adapter{servers: servers})
}
