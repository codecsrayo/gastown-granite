package web

import (
	"bufio"
	"bytes"
	"context"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/quota"
)

// setupStreamTown creates a town root with mayor/ and seeds accounts.json so
// computeQuotaSnapshot has something to walk.
func setupStreamTown(t *testing.T, handles ...string) string {
	t.Helper()
	root := t.TempDir()
	if err := os.MkdirAll(filepath.Join(root, constants.DirMayor), 0o755); err != nil {
		t.Fatalf("mkdir mayor: %v", err)
	}
	acctCfg := &config.AccountsConfig{Default: handles[0], Accounts: map[string]config.Account{}}
	for _, h := range handles {
		acctCfg.Accounts[h] = config.Account{Email: h + "@example.com", ConfigDir: filepath.Join(root, h)}
	}
	if err := config.SaveAccountsConfig(constants.MayorAccountsPath(root), acctCfg); err != nil {
		t.Fatalf("save accounts: %v", err)
	}
	mgr := quota.NewManager(root)
	state, _ := mgr.Load()
	mgr.EnsureAccountsTracked(state, acctCfg.Accounts)
	if err := mgr.Save(state); err != nil {
		t.Fatalf("save quota: %v", err)
	}
	return root
}

func TestSnapshotHash_IgnoresGeneratedAt(t *testing.T) {
	base := QuotaSummaryResponse{
		GeneratedAt: "2026-05-25T12:00:00Z",
		Counters:    QuotaSummaryCounters{Available: 2, Limited: 1},
		Accounts: []QuotaSummaryAccount{
			{Handle: "work", Status: "available"},
			{Handle: "personal", Status: "limited"},
		},
	}
	h1, _, err := snapshotHash(base)
	if err != nil {
		t.Fatalf("hash1: %v", err)
	}

	moved := base
	moved.GeneratedAt = "2026-05-25T12:00:30Z"
	h2, _, err := snapshotHash(moved)
	if err != nil {
		t.Fatalf("hash2: %v", err)
	}
	if h1 != h2 {
		t.Errorf("hash differs solely due to GeneratedAt: %s vs %s", h1, h2)
	}

	changed := base
	changed.Counters.Available = 3
	h3, _, err := snapshotHash(changed)
	if err != nil {
		t.Fatalf("hash3: %v", err)
	}
	if h1 == h3 {
		t.Errorf("hash unchanged after Counters change")
	}
}

// flushRecorder wraps httptest.ResponseRecorder to satisfy http.Flusher, which
// the SSE handler requires. The recorded buffer is reused by tests to count
// emitted frames.
type flushRecorder struct {
	*httptest.ResponseRecorder
	mu       sync.Mutex
	flushes  int
	notifier chan struct{}
}

func newFlushRecorder() *flushRecorder {
	return &flushRecorder{ResponseRecorder: httptest.NewRecorder(), notifier: make(chan struct{}, 16)}
}

func (f *flushRecorder) Flush() {
	f.mu.Lock()
	f.flushes++
	f.mu.Unlock()
	select {
	case f.notifier <- struct{}{}:
	default:
	}
}

func (f *flushRecorder) snapshotEvents() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	out := []string{}
	scanner := bufio.NewScanner(bytes.NewReader(f.Body.Bytes()))
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "event: ") {
			out = append(out, strings.TrimPrefix(line, "event: "))
		}
	}
	return out
}

func TestStreamQuotaSnapshots_EmitsInitialThenDedupes(t *testing.T) {
	town := setupStreamTown(t, "work")
	rec := newFlushRecorder()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan struct{})
	go func() {
		streamQuotaSnapshots(ctx, rec, rec, town, 50*time.Millisecond, time.Hour)
		close(done)
	}()

	// Wait for the initial flush (connected + initial snapshot).
	select {
	case <-rec.notifier:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for initial flush")
	}

	// Now let ~10 ticks fire with no state changes — dedupe should suppress
	// every one of them, so no further flushes should arrive.
	time.Sleep(600 * time.Millisecond)
	cancel()
	<-done

	events := rec.snapshotEvents()
	snapshots := 0
	for _, ev := range events {
		if ev == "quota-snapshot" {
			snapshots++
		}
	}
	if snapshots != 1 {
		t.Errorf("got %d quota-snapshot frames over 10 idle ticks, want exactly 1 (dedupe should suppress repeats); events=%v",
			snapshots, events)
	}
}

func TestStreamQuotaSnapshots_EmitsNewFrameAfterStateChange(t *testing.T) {
	town := setupStreamTown(t, "work", "personal")
	rec := newFlushRecorder()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan struct{})
	go func() {
		streamQuotaSnapshots(ctx, rec, rec, town, 30*time.Millisecond, time.Hour)
		close(done)
	}()

	// Wait for the initial frame.
	select {
	case <-rec.notifier:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for initial flush")
	}

	// Flip an account's status — next tick (or fsnotify fire) should emit.
	mgr := quota.NewManager(town)
	state, err := mgr.Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	acct := state.Accounts["personal"]
	acct.Status = config.QuotaStatusLimited
	acct.ResetsAt = "7pm"
	state.Accounts["personal"] = acct
	if err := mgr.Save(state); err != nil {
		t.Fatalf("save: %v", err)
	}

	// Drain notifier and wait for a snapshot frame after the change.
	deadline := time.After(1500 * time.Millisecond)
	for {
		select {
		case <-rec.notifier:
			body := rec.Body.String()
			// Count "quota-snapshot" frames — should be >=2 (initial + post-change).
			if strings.Count(body, "event: quota-snapshot") >= 2 && strings.Contains(body, `"limited":1`) {
				cancel()
				<-done
				return
			}
		case <-deadline:
			cancel()
			<-done
			t.Fatalf("did not observe post-change snapshot; body=%s", rec.Body.String())
		}
	}
}
