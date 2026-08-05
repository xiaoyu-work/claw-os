# Network Diagnosis

Use for offline, slow, DNS, latency, dropped connection, or port complaints.

## Initial evidence

```json
{"command":"run","args":["--domain","network","network is slow"]}
```

Inspect:

- `network`: local interfaces, errors, TCP states, listening sockets;
- `network-rate`: sampled interface throughput;
- `failed-units`: failed network-related services;
- `load`: host pressure that may masquerade as network slowness.

## Decision tree

1. **No interface is visible**
   - Check the NetworkManager service and device/driver state.
2. **RX/TX errors are increasing**
   - Suspect link, driver, Wi-Fi signal, cable, or virtual-interface issues.
3. **High throughput**
   - Identify whether the traffic is expected before throttling or stopping it.
4. **Many `SYN_SENT` connections**
   - The remote path, DNS result, routing, or firewall may be failing.
5. **Only one destination fails**
   - Check `cos_netfilter check` for the exact host.
   - Use the `net` or `web` app for a capability-gated application request.
6. **All destinations fail**
   - Treat link, route, DNS, VPN, or NetworkManager as more likely than an app
     bug.

## Current limitation

The current toolset observes local counters but does not yet provide active
ping, traceroute, route, DNS, or NetworkManager control. State this limitation
instead of claiming the remote network is healthy.

Do not reset networking or disconnect a VPN without explaining the impact and
obtaining approval.
