// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"crypto/md5"
	"encoding/hex"
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
	rootUser  = "harness"
	rootPass  = "harness-secret-0123456789"
	region    = "us-east-1"
	nodeCount = 4
)

type adapter struct {
	servers []*minio.Client
}

func (adapter) Family() family.Family { return family.Register{} }

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
		Creds:        credentials.NewStaticV4(rootUser, rootPass, ""),
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

// Every MinIO node serves the S3 API and takes the namespace lock and the
// drive quorum for the request, so each operation goes to one node drawn from
// the SDK stream.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	s := c.servers[sdk.Below(uint64(len(c.servers)))]
	switch op.F {
	case family.RegisterWrite:
		return nil, classify(put(ctx, s, op.K, op.V.(string), minio.PutObjectOptions{}))
	case family.RegisterCAS:
		pair := op.V.([]string)
		opts := minio.PutObjectOptions{}
		opts.SetMatchETag(etag(pair[0]))
		return nil, classifyCAS(put(ctx, s, op.K, pair[1], opts))
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

// MinIO gives an object written in one unencrypted, uncompressed PUT the
// hex MD5 of its content as ETag. Values are unique per operation, so the
// ETag names the one write that stored the value.
func etag(value string) string {
	sum := md5.Sum([]byte(value))
	return hex.EncodeToString(sum[:])
}

func put(ctx context.Context, s *minio.Client, key, value string, opts minio.PutObjectOptions) error {
	data := []byte(value)
	opts.DisableMultipart = true
	_, err := s.PutObject(ctx, bucket, key, bytes.NewReader(data), int64(len(data)), opts)
	return err
}

func get(ctx context.Context, s *minio.Client, key string) (*string, error) {
	object, err := s.GetObject(ctx, bucket, key, minio.GetObjectOptions{})
	if err != nil {
		return nil, err
	}
	defer object.Close()
	data, err := io.ReadAll(object)
	if err != nil {
		if fromServer(err, http.StatusNotFound, minio.NoSuchKey) {
			return nil, nil
		}
		return nil, err
	}
	text := string(data)
	return &text, nil
}

// Only a failed dial is definite for a write. A quorum error, a timeout, a
// redirect or a reset can follow a write that reached some drives, and a
// later read can still return it.
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

// MinIO evaluates If-Match under the object's namespace lock before it writes
// any drive, and answers 412 when the ETag differs or 404 when the object is
// missing.
func classifyCAS(err error) error {
	if err == nil {
		return nil
	}
	if fromServer(err, http.StatusPreconditionFailed, "PreconditionFailed") || fromServer(err, http.StatusNotFound, minio.NoSuchKey) {
		return runner.Definite(err)
	}
	return classify(err)
}

// minio-go invents a NoSuchKey or PreconditionFailed error for a 404 or 412
// answer without an XML body. Only MinIO's own XML error, which names the
// resource, says what the server did.
func fromServer(err error, status int, code string) bool {
	resp := minio.ToErrorResponse(err)
	return resp.StatusCode == status && resp.Code == code && resp.Resource != ""
}

func (a adapter) Ready(ctx context.Context) error {
	nodes := make([]int, len(a.servers))
	for i := range nodes {
		nodes[i] = i
	}
	return runner.AnyMember(ctx, nodes, func(ctx context.Context, i int) error {
		s := a.servers[i]
		if err := s.MakeBucket(ctx, bucket, minio.MakeBucketOptions{Region: region}); err != nil {
			code := minio.ToErrorResponse(err).Code
			if code != "BucketAlreadyOwnedByYou" && code != "BucketAlreadyExists" {
				return fmt.Errorf("n%d: make bucket: %w", i+1, err)
			}
		}
		if err := put(ctx, s, "ready", "1", minio.PutObjectOptions{}); err != nil {
			return fmt.Errorf("n%d: %w", i+1, err)
		}
		return nil
	})
}

func main() {
	servers := make([]*minio.Client, nodeCount)
	for i := range servers {
		servers[i] = newServer(fmt.Sprintf("10.77.0.%d:9000", i+1))
	}
	runner.Main(adapter{servers: servers})
}
