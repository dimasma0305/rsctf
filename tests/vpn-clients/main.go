// Disposable, secret-free-in-logs transport fixture. Not part of the application.
package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

	"golang.org/x/crypto/curve25519"
	"golang.org/x/net/dns/dnsmessage"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun/netstack"
)

type fixture struct {
	Schema           int    `json:"schema"`
	SourceSHA        string `json:"sourceSha"`
	ServerPrivateKey string `json:"serverPrivateKey"`
	HubAddress       string `json:"hubAddress"`
	ServiceAddress   string `json:"serviceAddress"`
	IngressAddress   string `json:"ingressAddress"`
	Hostname         string `json:"hostname"`
	Cases            []struct {
		Name     string   `json:"name"`
		Profiles []string `json:"profiles"`
	} `json:"cases"`
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}
func key(s string) []byte {
	b, e := base64.StdEncoding.DecodeString(s)
	must(e)
	if len(b) != 32 {
		panic("invalid key length")
	}
	return b
}
func public(s string) string {
	b, e := curve25519.X25519(key(s), curve25519.Basepoint)
	must(e)
	return hex.EncodeToString(b)
}
func field(s, name string) string {
	for _, line := range strings.Split(s, "\n") {
		if v, ok := strings.CutPrefix(strings.TrimSpace(line), name+" = "); ok {
			return v
		}
	}
	panic("missing profile field: " + name)
}
func profileIP(s string) string { return strings.Split(field(s, "Address"), "/")[0] }
func newDevice(addresses []string, ipc string) (*device.Device, *netstack.Net) {
	ips := make([]netip.Addr, len(addresses))
	for i, a := range addresses {
		ips[i] = netip.MustParseAddr(a)
	}
	tun, n, e := netstack.CreateNetTUN(ips, nil, 1420)
	must(e)
	d := device.NewDevice(tun, &loopbackBind{}, device.NewLogger(device.LogLevelError, "vpn-ci: "))
	must(d.IpcSet(ipc))
	must(d.Up())
	return d, n
}
func client(profile string) (*device.Device, *netstack.Net) {
	ipc := "private_key=" + hex.EncodeToString(key(field(profile, "PrivateKey"))) + "\npublic_key=" + hex.EncodeToString(key(field(profile, "PublicKey"))) + "\nendpoint=" + field(profile, "Endpoint") + "\npersistent_keepalive_interval=1\n"
	for _, route := range strings.Split(field(profile, "AllowedIPs"), ",") {
		ipc += "allowed_ip=" + strings.TrimSpace(route) + "\n"
	}
	return newDevice([]string{profileIP(profile)}, ipc)
}
func probe(n *netstack.Net, address string, sequence uint64) error {
	payload := []byte(fmt.Sprintf("teammate-%d", sequence))
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c, err := n.DialContext(ctx, "tcp", net.JoinHostPort(address, "2222"))
	if err != nil {
		return err
	}
	defer c.Close()
	_ = c.SetDeadline(time.Now().Add(2 * time.Second))
	if _, err = c.Write(payload); err != nil {
		return err
	}
	reply := make([]byte, len(payload))
	if _, err = io.ReadFull(c, reply); err != nil {
		return err
	}
	if !bytes.Equal(reply, payload) {
		return fmt.Errorf("TCP mismatch")
	}
	u, err := n.DialUDPAddrPort(netip.AddrPort{}, netip.AddrPortFrom(netip.MustParseAddr(address), 9999))
	if err != nil {
		return err
	}
	defer u.Close()
	_ = u.SetDeadline(time.Now().Add(2 * time.Second))
	if _, err = u.Write(payload); err != nil {
		return err
	}
	reply = make([]byte, 256)
	length, err := u.Read(reply)
	if err != nil {
		return err
	}
	if !bytes.Equal(reply[:length], payload) {
		return fmt.Errorf("UDP mismatch")
	}
	return nil
}
func serveEcho(n *netstack.Net, serviceAddress string) {
	l, e := n.ListenTCP(&net.TCPAddr{Port: 2222})
	must(e)
	go func() {
		for {
			c, e := l.Accept()
			if e != nil {
				return
			}
			go func() { defer c.Close(); _ = c.SetDeadline(time.Now().Add(5 * time.Second)); _, _ = io.Copy(c, c) }()
		}
	}()
	u, e := n.ListenUDP(&net.UDPAddr{IP: net.ParseIP(serviceAddress), Port: 9999})
	must(e)
	go func() {
		b := make([]byte, 4096)
		for {
			length, a, e := u.ReadFrom(b)
			if e != nil {
				return
			}
			_, _ = u.WriteTo(b[:length], a)
		}
	}()
}
func serveDNS(n *netstack.Net, f fixture) {
	u, e := n.ListenUDP(&net.UDPAddr{IP: net.ParseIP(f.HubAddress), Port: 53})
	must(e)
	go func() {
		b := make([]byte, 4096)
		for {
			size, a, e := u.ReadFrom(b)
			if e != nil {
				return
			}
			var query dnsmessage.Message
			if query.Unpack(b[:size]) != nil {
				continue
			}
			reply := dnsmessage.Message{Header: dnsmessage.Header{ID: query.ID, Response: true, Authoritative: true}, Questions: query.Questions}
			for _, q := range query.Questions {
				if strings.EqualFold(q.Name.String(), f.Hostname+".") && q.Type == dnsmessage.TypeA {
					reply.Answers = append(reply.Answers, dnsmessage.Resource{Header: dnsmessage.ResourceHeader{Name: q.Name, Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET, TTL: 0}, Body: &dnsmessage.AResource{A: netip.MustParseAddr(f.IngressAddress).As4()}})
				}
			}
			packet, e := reply.Pack()
			if e == nil {
				_, _ = u.WriteTo(packet, a)
			}
		}
	}()
}
func main() {
	if len(os.Args) != 4 {
		panic("usage: vpn-fixture fixture.json case-index ready.json")
	}
	raw, e := os.ReadFile(os.Args[1])
	must(e)
	var f fixture
	must(json.Unmarshal(raw, &f))
	if f.Schema != 1 {
		panic("fixture schema")
	}
	index, e := strconv.Atoi(os.Args[2])
	must(e)
	profiles := f.Cases[index].Profiles
	if len(profiles) != 2 || field(profiles[0], "PrivateKey") == field(profiles[1], "PrivateKey") || profileIP(profiles[0]) == profileIP(profiles[1]) {
		panic("teammates share an identity")
	}
	ipc := "private_key=" + hex.EncodeToString(key(f.ServerPrivateKey)) + "\nlisten_port=51822\n"
	for _, p := range profiles {
		ipc += "public_key=" + public(field(p, "PrivateKey")) + "\nallowed_ip=" + field(p, "Address") + "\n"
	}
	hub, n := newDevice([]string{f.HubAddress, f.ServiceAddress, f.IngressAddress}, ipc)
	defer hub.Close()
	serveEcho(n, f.ServiceAddress)
	serveDNS(n, f)
	companion, other := client(profiles[1])
	defer companion.Close()
	// Warmup is bounded. After it succeeds, every companion failure fails CI.
	deadline := time.Now().Add(10 * time.Second)
	for {
		err := probe(other, f.ServiceAddress, 0)
		if err == nil {
			break
		}
		if time.Now().After(deadline) {
			panic(fmt.Errorf("companion did not connect: %w", err))
		}
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	var successes, failures atomic.Uint64
	go func() {
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if probe(other, f.ServiceAddress, successes.Load()+1) != nil {
					failures.Add(1)
				} else {
					successes.Add(1)
				}
			}
		}
	}()
	control, e := net.Listen("tcp4", "127.0.0.1:0")
	must(e)
	mux := http.NewServeMux()
	mux.HandleFunc("GET /stats", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		state, err := hub.IpcGet()
		if err != nil {
			http.Error(w, "cannot read hub state", http.StatusInternalServerError)
			return
		}
		var handshakes uint64
		for _, line := range strings.Split(state, "\n") {
			if value, ok := strings.CutPrefix(line, "last_handshake_time_sec="); ok && value != "0" {
				handshakes++
			}
		}
		// Do not serialize the UAPI state: it includes private keys.
		_ = json.NewEncoder(w).Encode(map[string]uint64{"companionSuccesses": successes.Load(), "companionFailures": failures.Load(), "peersWithHandshake": handshakes})
	})
	server := &http.Server{Handler: mux, ReadHeaderTimeout: 2 * time.Second}
	defer server.Close()
	go func() { _ = server.Serve(control) }()
	ready, e := json.Marshal(map[string]string{"control": "http://" + control.Addr().String()})
	must(e)
	must(os.WriteFile(os.Args[3], ready, 0600))
	<-ctx.Done()
}
