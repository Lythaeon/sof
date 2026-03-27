#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/in.h>
#include <linux/ip.h>
#include <linux/udp.h>

#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

struct {
    __uint(type, BPF_MAP_TYPE_XSKMAP);
    __uint(max_entries, 64);
    __type(key, __u32);
    __type(value, __u32);
    __uint(pinning, LIBBPF_PIN_BY_NAME);
} xsks_map SEC(".maps");

static __always_inline int should_redirect_udp_port(__u16 port)
{
    return (port >= 12000 && port <= 12100) || port == 8001;
}

SEC("xdp")
int sof_xdp_port_redirect(struct xdp_md *ctx)
{
    void *data = (void *)(long)ctx->data;
    void *data_end = (void *)(long)ctx->data_end;

    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end) {
        return XDP_PASS;
    }
    if (eth->h_proto != bpf_htons(ETH_P_IP)) {
        return XDP_PASS;
    }

    struct iphdr *ip = (void *)(eth + 1);
    if ((void *)(ip + 1) > data_end) {
        return XDP_PASS;
    }
    if (ip->ihl < 5 || ip->protocol != IPPROTO_UDP) {
        return XDP_PASS;
    }
    if ((ip->frag_off & bpf_htons(0x3FFF)) != 0) {
        return XDP_PASS;
    }

    struct udphdr *udp = (void *)ip + (ip->ihl * 4);
    if ((void *)(udp + 1) > data_end) {
        return XDP_PASS;
    }

    if (!should_redirect_udp_port(bpf_ntohs(udp->dest))) {
        return XDP_PASS;
    }

    return bpf_redirect_map(&xsks_map, ctx->rx_queue_index, XDP_PASS);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
