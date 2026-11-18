package main

import (
	"fmt"
	"golang.zx2c4.com/wireguard/conn"
	"net"
	"net/netip"
	"sync"
)

// Keep the disposable hub and the independent teammate entirely on loopback.
// A separate IP stack models a second machine without conflicting OS routes.
type loopbackBind struct {
	socket *net.UDPConn
	mutex  sync.RWMutex
}
type endpoint struct{ netip.AddrPort }

func (e endpoint) ClearSrc()                 {}
func (e endpoint) SrcToString() string       { return "" }
func (e endpoint) DstToString() string       { return e.String() }
func (e endpoint) DstToBytes() []byte        { b, _ := e.MarshalBinary(); return b }
func (e endpoint) DstIP() netip.Addr         { return e.Addr() }
func (e endpoint) SrcIP() netip.Addr         { return netip.Addr{} }
func (b *loopbackBind) BatchSize() int       { return 1 }
func (b *loopbackBind) SetMark(uint32) error { return nil }
func (b *loopbackBind) ParseEndpoint(s string) (conn.Endpoint, error) {
	a, err := netip.ParseAddrPort(s)
	if err != nil {
		return nil, err
	}
	if !a.Addr().IsLoopback() {
		return nil, fmt.Errorf("non-loopback endpoint")
	}
	return endpoint{a}, nil
}
func (b *loopbackBind) Open(port uint16) ([]conn.ReceiveFunc, uint16, error) {
	b.mutex.Lock()
	defer b.mutex.Unlock()
	if b.socket != nil {
		return nil, 0, conn.ErrBindAlreadyOpen
	}
	s, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: int(port)})
	if err != nil {
		return nil, 0, err
	}
	b.socket = s
	receive := func(p [][]byte, sizes []int, endpoints []conn.Endpoint) (int, error) {
		n, a, err := s.ReadFromUDPAddrPort(p[0])
		if err != nil {
			return 0, err
		}
		sizes[0] = n
		endpoints[0] = endpoint{a}
		return 1, nil
	}
	return []conn.ReceiveFunc{receive}, uint16(s.LocalAddr().(*net.UDPAddr).Port), nil
}
func (b *loopbackBind) Close() error {
	b.mutex.Lock()
	defer b.mutex.Unlock()
	if b.socket == nil {
		return nil
	}
	err := b.socket.Close()
	b.socket = nil
	return err
}
func (b *loopbackBind) Send(p [][]byte, e conn.Endpoint) error {
	b.mutex.RLock()
	defer b.mutex.RUnlock()
	if b.socket == nil {
		return net.ErrClosed
	}
	for _, packet := range p {
		if _, err := b.socket.WriteToUDPAddrPort(packet, e.(endpoint).AddrPort); err != nil {
			return err
		}
	}
	return nil
}
