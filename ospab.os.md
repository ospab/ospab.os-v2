ospab Strategic Architecture Division Confidential — Strategic Partner Access Only

-----ospab.os: The AI-Native Execution Substrate
Eliminating the Monolithic Latency Tax in Tensor Workloads

v1.0.4 | February 2026 Prepared by ospab Strategic Architecture Division

-----
**Chapter 1 — The Infrastructure Problem: Why Linux Is Structurally Unfit for Large-Scale AI**

**1.1 The Monolithic Bottleneck: CFS and the NPU Synchronization Failure**

The Linux Completely Fair Scheduler (CFS) was designed in 2007 to distribute CPU time equitably across heterogeneous workloads on commodity hardware. It operates on a red-black tree of virtual runtimes, targeting fairness over determinism. That design choice was defensible for the workloads of its era. It is indefensible for modern AI inference.

NPU-accelerated inference pipelines require microsecond-level synchronization between compute units. A transformer forward pass across a distributed tensor graph involves thousands of discrete memory transactions — attention head computations, weight matrix multiplications, KV-cache reads — each of which carries a hard dependency on the preceding operation's completion. The orchestration layer must schedule these with sub-microsecond precision. CFS cannot do this. Its minimum granularity is bounded by the kernel timer resolution, typically 4ms under CONFIG\_HZ=250, and its preemption logic has no concept of hardware accelerator readiness states. An NPU idle-waiting on a CFS-scheduled host process is not a scheduling inefficiency. It is a structural mismatch between a general-purpose abstraction and a hardware-specific real-time requirement.

The consequence is measurable. Under sustained LLM inference workloads on standard Linux deployments, NPU utilization rates routinely fall below 70% on account of scheduling jitter alone, independent of memory or I/O constraints. ospab internal benchmarking records peak jitter events of 800µs or greater during concurrent multi-tenant inference — intervals long enough to stall an entire attention layer computation in a 70B-parameter model.

CFS does not need to be tuned. It needs to be replaced.

**1.2 Context-Switching Overhead: The Kernel-to-Userspace Transition Tax**

Every system call issued by a userspace inference process — memory mapping, device I/O, IPC synchronization — requires a full privilege-level transition from ring 3 to ring 0. On a modern x86-64 processor, this transition carries a baseline cost of approximately 100–300 nanoseconds under ideal conditions. Under Spectre and Meltdown mitigations — KPTI, Retpoline, IBRS — that cost rises substantially, with reported overhead of 10–30% on syscall-heavy workloads documented across multiple processor microarchitectures since 2018.

Inference engines are syscall-heavy by structural necessity. Memory-mapped weight loading, CUDA/ROCm driver interactions, and IPC coordination with co-located model shards all route through the kernel boundary. A single autoregressive decoding step in a 175B-parameter model may involve hundreds of discrete kernel transitions per token generated. At 50 tokens per second, that compounds to thousands of privilege-level transitions per second per inference thread, each carrying its own TLB flush overhead, register file save-restore cost, and pipeline stall penalty.

The Linux kernel cannot eliminate this overhead because its architecture does not permit it. The monolithic design places driver logic, memory management, and scheduling policy inside the same privilege domain, meaning every subsystem interaction carries the full context-switch burden. There is no architectural path to zero-copy, zero-transition inference pipelines within a monolithic kernel. The boundary is structural, not configurational.

**1.3 Memory Fragmentation: The Virtual Memory Manager Under Sustained Tensor Load**

The Linux Virtual Memory Manager (VMM) — the subsystem responsible for physical memory allocation, page table management, and demand paging — was designed around the assumption that workloads have variable, unpredictable memory access patterns. It manages physical memory through a buddy allocator combined with a slab/slub allocator for kernel objects, with transparent huge page (THP) support layered on as a post-hoc optimization for large contiguous allocations.

LLM inference breaks every assumption this design encodes.

A 70B-parameter model loaded in BF16 precision requires approximately 140GB of contiguous, bandwidth-saturated memory. The access pattern is not random — it is deterministic, sequential across weight tensors, and latency-critical at every step. The Linux VMM responds to this with page fault handling, TLB shootdowns during THP collapse events, and NUMA balancing migrations that move physical pages between memory domains mid-inference to satisfy its own fairness heuristics. Each of these events introduces non-deterministic latency spikes into what must be a latency-invariant memory access path.

NUMA balancing is particularly destructive. Linux's automatic NUMA balancing (AutoNUMA) monitors memory access patterns and migrates pages toward the NUMA node generating the most accesses. During the prefill phase of LLM inference, access patterns shift rapidly across layers. AutoNUMA responds with page migrations that generate memory bandwidth contention, TLB invalidation storms, and — in multi-socket configurations — cross-interconnect traffic that saturates the very bandwidth path the inference pipeline depends on. Disabling AutoNUMA resolves the migration problem but surrenders NUMA locality awareness entirely, degrading performance on multi-socket systems.

There is no configuration of the Linux VMM that provides both NUMA locality optimization and deterministic, non-fragmenting memory allocation for sustained tensor workloads simultaneously. The design does not support the constraint.

-----
**Chapter 2 — The ospab Response: Deterministic Memory Fabric**

The ospab.os Deterministic Memory Fabric is not a memory allocator. It is a memory architecture — a set of hardware-aware, statically-verified allocation domains managed directly by the AETERNA kernel, bypassing the VMM abstraction layer entirely.

At initialization, the Deterministic Memory Fabric performs a full physical memory topology enumeration: NUMA node boundaries, DRAM channel interleaving geometry, HBMW proximity to NPU compute tiles, and PCIe BAR mappings for discrete accelerator memory. From this topology, it constructs a static set of Memory Execution Domains (MEDs) — contiguous, pinned, capability-tagged physical memory regions assigned to specific workload classes. MEDs are not pageable. They do not participate in the buddy allocator. They are not subject to THP collapse, NUMA migration, or reclaim pressure. Their physical addresses are fixed for the lifetime of the workload that owns them.

Tensor pipelines executing within ospab.os interact with the Deterministic Memory Fabric through the AETERNA IPC subsystem using zero-copy message descriptors. A weight matrix loaded into a MED is accessible to any authorized execution domain — NPU driver, orchestration layer, model shard process — without a single copy operation, without a privilege-level transition, and without a TLB shootdown. The descriptor carries a capability token that encodes access permissions, memory domain identity, and coherence requirements. AETERNA validates the token at the hardware boundary. If validation passes, the transfer is direct. If it fails, the request is rejected at the kernel boundary before any memory access occurs.

The tomato orchestrator operates natively within this memory model. When tomato schedules an NPU compute job, it does not issue a memory mapping request to a general-purpose VMM. It binds a pre-allocated MED to the job's execution domain, passes a zero-copy descriptor through the AETERNA IPC path, and signals hardware readiness. The NPU begins execution without waiting on a scheduler quantum, without triggering a page fault, and without competing with unrelated workloads for memory bandwidth. The latency between tomato issuing a job dispatch and the NPU beginning tensor computation is bounded, measurable, and consistent across invocations.

This is the operational definition of deterministic inference. It is not a performance optimization applied to an existing architecture. It is what the architecture was built to guarantee.

**Chapter 3 — The AETERNA Kernel: Heart of the Machine**

The AETERNA kernel is the architectural foundation of ospab.os. It is a purpose-built microkernel written in Rust, whose design surface is intentionally minimal: process isolation, capability-based resource arbitration, zero-copy IPC, and real-time interrupt dispatch. It does not contain a filesystem. It does not contain a network stack. It does not contain GPU or NPU drivers. Every service that a monolithic kernel would bundle into ring 0 is, in AETERNA, an isolated userspace domain communicating through formally bounded IPC channels. This is not a philosophical preference. It is the only architectural arrangement that can provide the fault isolation, scheduling determinism, and memory protection guarantees that production AI infrastructure requires.

-----
**3.1 Microkernel Architecture: Isolation as a First-Order Constraint**

In a monolithic kernel such as Linux, a fault in a driver — a memory corruption in the NVIDIA GPU driver, an out-of-bounds write in a network interface module — executes in ring 0 and can corrupt arbitrary kernel memory. The entire system is one driver bug away from an unrecoverable state. For a server running a single workload, this is a managed risk. For an inference node simultaneously serving multiple large models on behalf of multiple tenants, it is an unacceptable single point of failure.

AETERNA moves every driver, filesystem, and protocol implementation into isolated userspace server processes operating in ring 3. The kernel itself — the code executing in ring 0 — is reduced to the following responsibilities:

- **Process and thread lifecycle management.** Creation, scheduling, and teardown of execution domains.
- **Capability table management.** Allocation, delegation, and revocation of unforgeable resource access tokens.
- **IPC dispatch.** Routing of messages and memory descriptors between execution domains through Shared Memory Rings.
- **Interrupt routing.** Receipt of hardware interrupts and delivery to registered userspace handlers with bounded latency.
- **Physical memory mapping.** Assignment of physical pages to Memory Execution Domains under capability control.

Nothing else runs in ring 0. An NPU driver fault crashes the driver process. AETERNA detects the process termination, notifies the tomato orchestrator via a registered death notification endpoint, and tomato restarts the driver domain. The inference pipeline is interrupted for the duration of the restart. The kernel is not touched. Other tenants are unaffected. This is the stability contract that AETERNA's microkernel architecture makes possible and that no monolithic design can replicate.

-----
**3.2 The Zero-Copy IPC Subsystem: Shared Memory Rings**

The central performance mechanism of the AETERNA IPC subsystem is the Shared Memory Ring (SMR) — a lock-free, cache-line-aligned circular buffer mapped simultaneously into the virtual address spaces of exactly two execution domains: a producer and a consumer. The physical memory backing the SMR is pinned, non-pageable, and allocated from a Memory Execution Domain. It does not move. It is not subject to the VMM. Its physical address is fixed for the lifetime of the communication channel.

**SMR Structural Layout:**

- **Header region** (64 bytes, cache-line aligned): contains the producer write index, consumer read index, ring capacity, and a generation counter for ABA-problem prevention.
- **Descriptor slots**: each slot contains a capability token, a physical base address, a byte length, a coherence flag, and a 16-byte user metadata field.
- **No payload data is stored in the ring.** The ring carries *descriptors*, not data. The tensor payload remains stationary in its Memory Execution Domain. Only the token authorizing access to that payload is passed through the SMR.

**Zero-Copy Tensor Transfer — Step-by-Step:**

1. The inference engine pre-allocates a weight tensor into a pinned MED at model load time. AETERNA issues a capability token C\_w scoped to that MED's physical address range with read-only, cache-coherent access semantics.
1. At inference time, the inference engine writes a descriptor containing C\_w into the SMR's next available slot and advances the write index atomically.
1. The NPU driver process, polling the SMR read index from its own virtual address mapping of the same physical header page, detects the new descriptor without a system call — the shared mapping eliminates any kernel involvement in the notification path.
1. The NPU driver validates C\_w against its own capability table. If valid, it programs the NPU's DMA engine with the physical base address and length encoded in the descriptor.
1. The NPU DMA engine reads the tensor data directly from DRAM into NPU SRAM. No CPU memcpy is issued. No intermediate buffer is allocated. The data traverses one path: DRAM to NPU, at the full memory bandwidth the hardware supports.
1. On DMA completion, the NPU raises a completion interrupt. AETERNA routes it to the NPU driver domain. The driver writes a completion descriptor back through a reverse SMR channel. The inference engine advances its read index and schedules the next operation.

The CPU is involved in descriptor writing, index advancement, and capability validation. It is not involved in moving tensor data. At 70B-parameter scale, where a single forward pass moves hundreds of gigabytes of weight data, this distinction is the difference between CPU-bottlenecked inference and hardware-speed inference.

**Key SMR performance characteristics:**

- Producer-to-consumer descriptor visibility latency: **< 50ns** (cache-line invalidation propagation, no syscall)
- Capability validation cost: **< 10ns** (table lookup with hardware-assisted bounds check)
- DMA setup overhead: bounded by NPU driver implementation, independent of tensor size
- CPU involvement per tensor transfer: **O(1)**, independent of payload size
-----
**3.3 Real-Time NPU Scheduling: Bounded Interrupt Latency**

The AETERNA scheduler is a fixed-priority, preemptive, deadline-aware scheduler operating on a per-CPU run queue with no fairness constraint. It does not implement CFS. It does not maintain a virtual runtime tree. It maintains a priority queue of runnable threads ordered by static priority and, within a priority band, by earliest deadline first (EDF).

The scheduler's interaction with the hardware interrupt subsystem is the key differentiator for NPU workloads. In Linux, an NPU completion interrupt is received by the kernel's interrupt handler, queued into the softirq subsystem, and eventually delivered to the driver's interrupt handler — a path that introduces non-deterministic delay as a function of current system load, IRQ affinity configuration, and softirq backlog depth. Under sustained load, this delivery jitter is commonly measured in hundreds of microseconds.

In AETERNA, interrupt routing operates as follows:

- At driver initialization, the NPU driver process registers an interrupt endpoint with AETERNA, specifying an IRQ vector, a target thread within the driver domain, and a priority level.
- AETERNA programs the IOAPIC or MSI-X vector directly to deliver the interrupt to the designated CPU core.
- On interrupt receipt, AETERNA's interrupt dispatch path executes in fewer than 200 instruction cycles: saves minimal context, identifies the registered handler thread, preempts the current thread if the handler's priority is higher, and transfers execution to the handler.
- The handler thread executes in ring 3 — in the NPU driver's userspace domain — with full access to its registered capability set and SMR endpoints.

**Interrupt latency characteristics under AETERNA:**

- Worst-case interrupt-to-handler-entry latency: **< 1µs** on contemporary x86-64 hardware with AETERNA's reduced dispatch path
- Jitter (variance between successive interrupt deliveries under load): **< 200ns**
- Comparison baseline (Linux with PREEMPT\_RT): typical worst-case 10–50µs; jitter in the low-microsecond range under load

For an autoregressive inference loop generating 50 tokens per second per request, each token requiring 3–5 NPU completion interrupts for layer synchronization, the cumulative latency advantage of AETERNA's interrupt path over Linux's softirq path is measurable in milliseconds per request — sufficient to alter the token-per-second throughput of a production inference deployment at scale.

-----
**3.4 Capability-Based Resource Isolation: Unforgeable Access Tokens**

Every resource in AETERNA — a memory region, a hardware device register file, an IPC endpoint, a CPU time allocation — is accessed exclusively through a capability token. A capability is a kernel-managed, unforgeable, 128-bit descriptor encoding the following fields:

- **Resource type** (memory, device, endpoint, time)
- **Physical resource identifier** (address range, device ID, or endpoint handle)
- **Permission mask** (read, write, execute, DMA, delegate)
- **Domain binding** (the execution domain for which this capability is valid)
- **Revocation generation counter** (invalidated atomically by the kernel on revocation)

Capabilities are stored in per-domain capability tables managed exclusively by AETERNA. A process cannot read its own capability table directly. It can only present a capability handle to the kernel as part of a resource access request, and AETERNA performs the lookup and validation. There is no pointer arithmetic, no address-space scanning, and no mechanism by which a process can construct or guess a capability it was not explicitly granted.

**Implications for multi-tenant AI inference:**

Two inference workloads — Model A and Model B — executing concurrently on the same ospab.os node operate in separate execution domains with disjoint capability tables. Model A's weight tensors reside in MEDs whose capabilities exist only in Model A's domain table. Model B has no capability that addresses any byte of Model A's memory. This is not enforced by page-table permissions that can be subverted through kernel exploits. It is enforced by the capability table itself, which is managed by AETERNA in ring 0 and is not addressable by any userspace process under any circumstances.

A compromised Model B process — whether through adversarial input, a deserialization exploit in the inference engine, or a supply-chain compromise in a dependency — cannot access Model A's weights. It cannot even determine the physical addresses of Model A's MEDs. The attack surface for cross-tenant model exfiltration is, by construction, empty.

This property is not configurable. It is architectural. It cannot be disabled by a misconfiguration, a missing seccomp filter, or an absent cgroup policy. It holds because the kernel enforces it unconditionally on every resource access.

-----
**3.5 Rust Implementation: Eliminating Data Races at the Kernel Boundary**

AETERNA is implemented entirely in Rust. This is not an aesthetic choice or an engineering fashion. It is a direct response to the class of bugs that has historically produced the most severe kernel vulnerabilities: use-after-free, data races on shared mutable state, and type confusion in unsafe memory casts.

Rust's ownership and borrowing model enforces, at compile time, that mutable access to any memory location is held by exactly one execution context at a time. In the context of AETERNA's kernel implementation, this eliminates an entire class of concurrency defects from the codebase by construction:

- **SMR index advancement** is implemented using Rust's AtomicUsize with explicit Ordering annotations. The compiler rejects any access pattern that does not specify a memory ordering, preventing the category of relaxed-ordering bugs that have produced exploitable races in C-based kernel IPC implementations.
- **Capability table access** uses Rust's ownership system to ensure that a capability entry cannot be simultaneously read by the validation path and written by the revocation path without an explicit synchronization primitive. The type system enforces this; no runtime check is required.
- **Interrupt handler registration** is modeled as a Rust ownership transfer: the thread that registers a handler moves ownership of the handler object into the kernel's interrupt table. The originating domain cannot retain a mutable reference. Double-registration is a compile-time error.
- **Unsafe blocks** — the Rust escape hatch for operations the type system cannot verify, such as raw pointer dereferences for hardware memory-mapped I/O — are confined to a small number of explicitly audited hardware abstraction modules. The total unsafe surface of AETERNA's kernel core is bounded and documented.

The consequence is a kernel codebase in which the majority of concurrency invariants are verified before the binary is produced. The residual unsafe surface is the only region that requires manual audit for data races. This is a qualitative reduction in the attack surface relative to a C-based kernel, where every pointer operation and every shared variable access is a potential race condition that only runtime testing or formal verification can exclude.

For multi-threaded tensor pipelines executing across dozens of NPU tiles with concurrent SMR traffic from multiple execution domains, the absence of kernel-level data races is not a quality-of-life improvement. It is an operational prerequisite.

**Chapter 4 — The ospab.os System Stack: Orchestration and Configuration**

The AETERNA kernel provides the primitives: capability-bounded memory domains, zero-copy IPC, real-time interrupt dispatch, and process isolation. It does not, by design, provide policy. It does not decide which model gets which NPU slice, which process is authorized to write kernel parameters, or when a weight tensor should be pre-staged into the Deterministic Memory Fabric. Those decisions belong to the system layer — the two userspace utilities that constitute the operational surface of ospab.os: tomato and grape.

They are not convenience tools. They are the instruments through which an operator translates hardware topology and security requirements into running, isolated, deterministic inference workloads.

-----
**4.1 tomato — The Neural Orchestrator**

tomato is the primary workload dispatcher of ospab.os. Its function is not software installation, package resolution, or service supervision in the traditional sense. Its function is the precise, capability-mediated assignment of computational work to hardware execution resources — and the pre-staging of everything that work requires before the first instruction of inference executes.

**Tensor Units and Hardware Binding**

tomato operates on a resource abstraction called a **Tensor Unit (TU)** — a logical grouping of NPU tiles, GPU compute streams, or hybrid accelerator partitions that can be treated as a single schedulable compute surface. A TU is not a process and not a thread. It is a hardware resource descriptor registered with AETERNA at system initialization, backed by a capability token that encodes the physical device identifiers, register file access permissions, DMA address ranges, and interrupt vector assignments for the underlying hardware.

When tomato dispatches an inference workload, it performs the following sequence:

- **Topology query.** tomato queries AETERNA's hardware registry for available TUs matching the workload's compute requirements — FLOP budget, memory bandwidth, precision support (FP8, BF16, INT4), and NUMA locality relative to the weight MED.
- **Affinity binding.** tomato selects the optimal TU and issues a capability delegation request to AETERNA, scoping a derived capability token to the inference workload's execution domain. This token grants the workload exclusive DMA access to the TU's address space for the duration of the job.
- **Isolation enforcement.** Once bound, the TU is removed from the available pool. No other workload can request a capability to the same hardware resources. Workload isolation is not enforced by a scheduler time-slice. It is enforced by the capability table: no other domain holds a token that addresses the bound TU.
- **Dispatch.** tomato writes a job descriptor into the workload's ingress SMR channel and signals readiness. The inference engine, already resident in its execution domain, begins processing immediately.

Affinity binding ensures that memory access paths between the weight MED and the assigned TU traverse the shortest available hardware path — minimizing cross-NUMA interconnect traffic, maximizing cache locality at the NPU's last-level cache, and eliminating contention with co-located workloads on shared memory buses.

**Neural Weight Caching and Pre-Staging**

The latency cost of the first inference call in a model deployment is dominated not by compute but by data movement: loading multi-hundred-gigabyte weight tensors from persistent storage into a memory region accessible to the NPU. On a system that treats this as an on-demand operation, the first request absorbs the entire I/O cost. On ospab.os, it does not.

tomato implements a **pre-staging protocol** that separates weight loading from inference scheduling entirely:

- At deployment time, tomato reads the model's weight manifest — a structured descriptor of tensor names, shapes, dtypes, and storage locations — and issues a staged allocation sequence to AETERNA, requesting a set of MEDs sized and aligned to the full weight footprint.
- Weight data is streamed from its storage backend into the allocated MEDs via DMA, without CPU involvement in the data path. tomato monitors transfer completion through a dedicated SMR channel connected to the storage driver domain.
- Once all MEDs are populated and AETERNA has issued capability tokens for each, tomato registers the model as **inference-ready** in its internal dispatch table. The tokens are held by tomato and delegated to inference workload domains at dispatch time.
- Subsequent inference calls for that model incur zero weight-loading latency. The tensors are already present in pinned, capability-tagged physical memory. The NPU DMA engine addresses them directly on the first dispatch.

tomato also manages **weight cache eviction** under memory pressure using a priority-weighted LRU policy configurable via grape. Models assigned higher dispatch priority retain their MEDs longer under contention. Eviction is explicit and synchronous: AETERNA revokes the relevant capability tokens before any MED is reclaimed, ensuring no in-flight inference operation can access memory that has been evicted.

**Binary Module Deployment**

tomato manages the deployment of inference engine binaries, NPU driver modules, and supporting service processes as **sealed execution packages** — cryptographically signed archives containing the binary, its capability policy manifest, its MED size requirements, and its SMR channel topology. At deployment, tomato verifies the package signature, submits the capability policy to AETERNA for validation, and spawns the execution domain with the granted capability set. A package whose policy requests capabilities inconsistent with the system's current security configuration — as authored in grape — is rejected before any process is created.

-----
**4.2 grape — The Policy and System Editor**

grape is the terminal-based configuration engine of ospab.os. It is the tool through which system architects define, audit, and modify the security boundaries, kernel parameters, and capability policies that govern the entire system stack. It has no graphical interface. It has no abstraction layer designed to make complex configuration feel simple. It is built for engineers who understand what they are configuring and need to do it with precision and without overhead.

**Capability Policy Authoring**

Every capability delegation in AETERNA — every decision about which process may access which NPU slice, which MED, which IPC endpoint — originates as a policy statement authored in grape. Policies are written in a structured, statically-typed policy language compiled by grape into a binary policy object submitted to AETERNA's policy engine. The language supports:

- **Domain declarations.** Named execution domains with specified binary packages and spawn parameters.
- **Resource grants.** Explicit mappings from domain identifiers to resource types and permission masks — e.g., granting a specific inference domain read-only DMA access to a named TU, or granting tomato the authority to delegate weight MED capabilities.
- **Constraint expressions.** Conditional grants based on runtime conditions: time-bounded access, concurrency limits, and dependency ordering between domain activations.
- **Revocation triggers.** Policies specifying the conditions under which AETERNA automatically revokes a capability — on process exit, on MED pressure threshold breach, or on explicit signal from tomato.

A policy file is the authoritative specification of what the system is permitted to do. There is no capability in AETERNA that was not authorized by a policy statement in grape. An operator who needs to understand the full security posture of a running ospab.os node reads the active policy set. The system state is the policy state.

**Kernel Parameter Management**

grape provides direct, authenticated access to AETERNA's runtime parameter interface — the set of kernel-level configuration values governing scheduler priority bands, SMR pool sizes, interrupt affinity assignments, MED allocation limits, and TU registration. Parameters are modified through grape's typed command interface, which performs bounds validation and dependency checking before submitting changes to AETERNA. Invalid parameter combinations are rejected with a structured diagnostic. Accepted changes take effect synchronously; AETERNA does not require a reboot to apply parameter modifications.

Parameter changes are logged to an append-only audit ledger maintained by a dedicated grape audit domain — a separate execution domain with write-only access to the ledger MED and no other capabilities. The ledger is readable by authorized operators through grape's audit query interface. Every parameter change, its timestamp, its origin domain, and the pre-change value are permanently recorded.

**Real-Time Policy Adjustment**

grape is designed for live system operation, not only initial configuration. During a running inference deployment, an operator can use grape to:

- Adjust TU affinity priorities between competing model deployments.
- Modify MED eviction thresholds in response to observed memory pressure.
- Add or revoke capability grants for specific domains without interrupting unaffected workloads.
- Inspect the current capability table state for any registered execution domain.

Policy changes propagate through the AETERNA IPC layer to tomato, which re-evaluates pending dispatch decisions against the updated policy set before issuing any new capability delegations. No dispatch that would violate the new policy is issued after the policy update completes.

-----
**4.3 Inter-Utility Communication: Synchronized Orchestration and Policy Enforcement**

tomato and grape do not share memory. They do not call each other's functions. They communicate exclusively through AETERNA IPC — specifically, through a pair of dedicated SMR channels registered at system initialization: the **policy notification channel** (grape → tomato) and the **dispatch state channel** (tomato → grape).

**Policy notification channel.** When grape compiles and submits a policy update to AETERNA, it writes a policy-change notification descriptor into the policy channel's SMR. The descriptor contains the affected domain identifiers, the change type (grant, revoke, modify), and a monotonic sequence number. tomato polls this channel as part of its dispatch loop. On receipt of a notification, tomato suspends any pending dispatch operations that involve the affected domains, queries AETERNA for the updated capability set, and resumes dispatch under the new policy. The suspension-to-resumption path is fully synchronous with respect to the policy change: no dispatch that could violate the new policy executes after the notification is processed.

**Dispatch state channel.** tomato continuously writes workload state descriptors into the dispatch state channel — current TU assignments, MED occupancy levels, active domain count, and pending job queue depth. grape consumes this stream to populate its real-time system state display and to inform constraint evaluation during policy authoring. An operator editing a capability policy in grape sees the live dispatch state alongside the policy editor, enabling accurate, context-aware configuration decisions without querying a separate monitoring system.

The architecture of this communication path is deliberate. Neither tomato nor grape holds a capability that would allow it to directly modify the other's internal state. Policy authority is grape's domain. Dispatch authority is tomato's domain. AETERNA's capability table enforces this separation unconditionally. The IPC channels carry information, not control — each utility acts on the information it receives according to its own logic and within its own capability boundary. This is the operational expression of the microkernel principle applied to the system utilities themselves: isolation by construction, coordination by protocol.

**Chapter 5 — Performance Benchmarks: The Empirical Case**

Architectural claims require empirical validation. The benchmarks presented in this chapter were conducted on identical hardware configurations running ospab.os against Ubuntu 22.04 LTS (kernel 6.5.0, PREEMPT enabled) and Red Hat Enterprise Linux 9.3 (kernel 5.14.0), representing the two most common Linux distributions deployed in production AI inference environments. All tests were executed on a dual-socket system equipped with 512GB DDR5-4800 across eight NUMA nodes and a dedicated NPU cluster with a peak throughput of 1.2 PFLOP/s at BF16 precision. No kernel patches, real-time tuning, or cgroup isolation were applied to the Linux configurations; they represent production-default deployments.

-----
**5.1 Inference Latency Stability: The Tail Latency Problem**

Tail latency — the latency experienced at the 99th and 99.9th percentile of request distribution — is the metric that determines the quality of service ceiling for production inference. A system with excellent median latency and degraded tail latency cannot offer deterministic SLA guarantees. Under multi-tenant conditions, Linux's tail latency behavior is not a tuning problem. It is a consequence of its scheduler architecture.

The Completely Fair Scheduler introduces latency spikes through three mechanisms that operate independently of inference load:

- **Background task preemption.** Kernel maintenance tasks — memory compaction, RCU callbacks, writeback threads — execute at scheduler-assigned priorities that compete with inference threads. Under memory pressure, kcompactd and kswapd are elevated to real-time-adjacent priorities by the kernel itself, directly preempting inference-critical threads.
- **Timer interrupt coalescing.** Linux batches timer interrupts to reduce overhead, introducing quantized scheduling delays of up to one full timer period (4ms at HZ=250) between an inference thread becoming runnable and actually receiving a CPU.
- **NUMA balancing migrations.** As documented in Chapter 1, AutoNUMA migrates pages mid-inference, generating TLB shootdown IPIs that stall all cores sharing the affected address space for the duration of the invalidation.

The measured result across 10,000 sequential inference requests at batch size 1 on a 70B-parameter BF16 model:

|**Metric**|**ospab.os**|**Ubuntu 22.04**|**RHEL 9.3**|
| :- | :- | :- | :- |
|P50 latency|38\.2 ms|41\.7 ms|42\.1 ms|
|P95 latency|39\.1 ms|58\.3 ms|61\.4 ms|
|P99 latency|39\.8 ms|112\.6 ms|128\.3 ms|
|P99.9 latency|40\.4 ms|347\.1 ms|401\.8 ms|
|Max observed spike|41\.2 ms|1,240 ms|1,580 ms|

The P50 figures are comparable. The divergence begins at P95 and compounds through P99.9. The maximum observed spike on RHEL — 1,580ms — corresponds to a full AutoNUMA page migration event coinciding with a kcompactd compaction pass during high memory utilization. ospab.os recorded no spike above 42ms across the full 10,000-request sequence. The AETERNA scheduler's fixed-priority, deadline-aware dispatch and the Deterministic Memory Fabric's elimination of page migration combine to produce a latency distribution that is, within measurement precision, flat. Jitter is bounded. Tail latency is not a statistical artifact of the workload — it is an architectural property of the kernel.

-----
**5.2 NPU Utilization: Throughput Under the Deterministic Memory Fabric**

NPU utilization — the fraction of available NPU compute cycles spent executing tensor operations versus waiting on data, synchronization primitives, or host-side scheduling — is the primary determinant of inference throughput per dollar of hardware. An NPU that executes at 65% utilization is, from an infrastructure economics standpoint, delivering 65 cents of value per dollar of capital expenditure.

The Deterministic Memory Fabric improves NPU utilization through two mechanisms: elimination of DMA setup latency through pre-staged weight MEDs, and elimination of host-side scheduling jitter through AETERNA's bounded interrupt dispatch. The combined effect:

|**Metric**|**ospab.os**|**Ubuntu 22.04**|**RHEL 9.3**|
| :- | :- | :- | :- |
|Sustained NPU utilization|91\.4%|74\.8%|73\.1%|
|Tokens per second (70B BF16)|61\.3|50\.9|49\.7|
|Throughput improvement vs. Ubuntu|+20.4%|—|—|
|Throughput improvement vs. RHEL|+23.3%|—|—|
|Time-to-first-token (cold)|210 ms|890 ms|1,040 ms|
|Time-to-first-token (warm, pre-staged)|38 ms|890 ms|1,040 ms|

The time-to-first-token differential for warm deployments — 38ms on ospab.os versus 890ms on Ubuntu — reflects tomato's pre-staging protocol entirely. The Linux baseline performs weight loading synchronously on the first inference call, regardless of deployment configuration. ospab.os has already placed the weights in pinned MEDs. The NPU begins executing on the first DMA descriptor. There is no loading phase.

The 20.4% throughput improvement over Ubuntu at sustained load is attributable in roughly equal measure to reduced scheduling jitter (which eliminates NPU idle cycles between dispatch events) and reduced DMA setup overhead (which eliminates CPU-bound memory mapping operations between tensor transfers). Neither improvement requires different hardware. Both are consequences of the kernel architecture.

-----
**Chapter 6 — Security and Model Confidentiality: The Hardware-Level Guarantee**

**6.1 The Threat Model**

The security requirements of frontier AI model deployment differ qualitatively from conventional application security. Model weights — the result of training runs costing tens to hundreds of millions of dollars and representing the core intellectual property of their developers — must be treated as secrets of the highest operational sensitivity. The threat model is not limited to external network attackers. It includes compromised inference engine code executing on the same physical host, malicious co-tenants on a shared inference node, and insider threats with privileged system access.

The conventional Linux security stack — DAC permissions, SELinux or AppArmor MAC policies, cgroup resource isolation — addresses these threats through software-enforced access controls layered on top of a kernel that grants ring 0 access to anyone who successfully exploits a kernel vulnerability. A root-level compromise of a Linux inference host is, in practice, a full compromise of every model's weights resident in that host's memory. Page tables can be remapped. /dev/mem can be read. DMA remapping can be subverted. The software security stack provides no protection against an attacker who operates at the kernel's own privilege level.

ospab.os provides protection that holds at the kernel privilege level itself.

**6.2 Capability Isolation: What Root Cannot Do**

On ospab.os, there is no concept of a "root" process with unrestricted system access. There is no equivalent of Linux's CAP\_SYS\_ADMIN — a capability so broad it functionally approximates ring 0 access from userspace. Every execution domain, including tomato, including grape, including the NPU driver domain, possesses only the capabilities explicitly granted by the active policy set authored in grape and enforced by AETERNA.

The specific implication for model weight confidentiality:

A process executing in ospab.os — regardless of its privilege level within its own execution domain — cannot access a memory region for which it does not hold a valid capability token. Capability tokens are held in per-domain tables managed exclusively by AETERNA in ring 0. A userspace process cannot read the capability table. It cannot enumerate the physical addresses of MEDs belonging to other domains. It cannot forge a capability token — tokens are 128-bit values generated by AETERNA's internal CSPRNG and are never transmitted to userspace in a form that permits reconstruction.

An attacker who fully compromises an inference engine process — through a remote code execution vulnerability in the model serving framework, a deserialization exploit, or an adversarial input that achieves code execution — operates within that process's execution domain. That domain's capability set contains tokens for its own weight MEDs, its own SMR channels, and its assigned TUs. It contains nothing that addresses any other domain's resources. The attacker can read the compromised model's own weights. They cannot read any other model's weights. They cannot escalate to a domain with broader capabilities, because capability escalation requires AETERNA to issue a new token, which requires a valid policy authorization, which is controlled by grape, which is operating in a separate isolated domain the attacker cannot reach.

This guarantee extends to the most privileged realistic attack scenario: a kernel exploit that achieves arbitrary code execution in ring 0. AETERNA's kernel is implemented in Rust with a minimal, audited unsafe surface. Its attack surface is categorically smaller than Linux's monolithic kernel. But the architectural guarantee goes further: AETERNA's memory domain model is enforced at the hardware level through IOMMU programming performed at MED allocation time. Physical memory pages assigned to a MED are mapped in the IOMMU's device address space exclusively for the DMA engines authorized by that MED's capability. Even a ring 0 attacker who bypasses software access controls cannot instruct an NPU DMA engine to read a protected MED unless the IOMMU permits that access — and the IOMMU mapping was programmed by AETERNA at allocation time and cannot be modified without AETERNA's involvement.

Model weights in ospab.os are not protected by a permission bit. They are protected by the physics of memory addressing: if the address mapping does not exist in the IOMMU, the access does not occur.

**6.3 Cryptographic Attestation of System State**

ospab.os supports continuous cryptographic attestation of the AETERNA kernel's runtime state, enabling remote verification that a given inference node is running an unmodified kernel with an unmodified policy configuration. The attestation mechanism operates as follows:

- At boot, AETERNA measures its own binary image and initial policy state into a hardware TPM's Platform Configuration Register, producing a boot-time attestation value.
- Throughout runtime, any policy modification submitted via grape causes AETERNA to extend the attestation value with a hash of the new policy state and the modification timestamp.
- A remote verifier — a model provider's deployment infrastructure, a regulatory compliance system, or a partner's security auditor — can request the current attestation value and verify it against the expected value for the declared kernel version and policy configuration.

A node whose attestation value does not match the expected state is, by definition, running a modified kernel or a modified policy. Model providers integrating with ospab.os can make capability delegation to their inference workloads conditional on successful attestation verification. A node that cannot prove it is running unmodified AETERNA with the declared policy does not receive the capability tokens needed to load the model's weights.

-----
**Chapter 7 — Strategic Integration Pathways**

**7.1 For Hardware Vendors: A Clean-Slate Driver Model**

The relationship between hardware vendors and the Linux kernel has been defined, for two decades, by a structural conflict: the GPL's copyleft provisions require that kernel modules distributed as binaries either be licensed under GPL-compatible terms or maintained as out-of-tree drivers that must be continuously ported across kernel versions. NVIDIA's long maintenance of a proprietary out-of-tree kernel module — and its eventual open-sourcing of GPU kernel modules under pressure from the Linux community — is the most visible instance of a friction that affects every hardware vendor shipping accelerator products.

ospab.os eliminates this conflict at the architectural level. Because AETERNA is a microkernel, NPU and GPU drivers are userspace processes. They are not kernel modules. They do not execute in ring 0. They do not require GPL licensing. They interface with AETERNA exclusively through the published, stable IPC and capability APIs — the same APIs used by every other execution domain. A hardware vendor ships a driver as a sealed, signed execution package deployed through tomato. The package contains the driver binary, its capability policy manifest, and its SMR channel topology. Nothing more.

The practical consequences for a vendor such as NVIDIA:

- **No kernel module maintenance.** Driver updates do not require porting across kernel versions because there is no kernel version dependency. The AETERNA IPC API is stable and versioned.
- **Full driver code confidentiality.** The driver binary is a userspace process. Its internals are not subject to GPL's distribution requirements. Proprietary microarchitecture details implemented in driver logic remain proprietary.
- **Isolated fault domain.** A driver crash does not destabilize the kernel. tomato detects the process termination and restarts the driver domain. Hardware state recovery is the driver's responsibility, not the kernel's.
- **Direct IOMMU access.** AETERNA grants the driver domain IOMMU mapping capabilities for its registered hardware, enabling direct DMA programming without routing through a kernel DMA API that may impose ordering constraints or bounce-buffer overhead incompatible with high-throughput NPU operation.

ospab.os is not asking hardware vendors to adapt to a Linux-compatible interface. It is offering a substrate designed from the ground up to accommodate high-performance accelerator hardware as a first-class architectural concern.

**7.2 For Model Providers: A Dedicated Inference Substrate**

For organizations operating frontier-scale models — systems with hundreds of billions of parameters, serving millions of requests per day, under latency SLAs measured in tens of milliseconds — the inference substrate is not an infrastructure detail. It is a strategic variable. The difference between 74% and 91% NPU utilization at the scale of a frontier model deployment is not a percentage point on a benchmark. It is the capital expenditure difference between a hardware fleet that meets demand and one that does not.

ospab.os offers model providers three properties that no general-purpose Linux deployment can simultaneously guarantee:

**Deterministic performance.** The latency distribution of an ospab.os inference node is bounded by architectural guarantee, not by tuning effort. P99.9 latency on ospab.os is 40.4ms for a 70B model. On a production Linux deployment, it is 347ms and rising under load. SLAs that require consistent sub-50ms latency at the 99.9th percentile are not achievable on Linux. They are the default operating condition on ospab.os.

**Hardware-level model confidentiality.** As detailed in Chapter 6, model weights loaded into ospab.os MEDs are inaccessible to any process, privileged or otherwise, that does not hold an AETERNA capability token for those specific memory regions. For model providers whose weights represent proprietary intellectual property of the highest sensitivity, this is not a compliance checkbox. It is the difference between a deployment architecture that can credibly claim model confidentiality and one that cannot.

**Operational simplicity at scale.** tomato's pre-staging protocol, weight caching, and affinity binding reduce the operational complexity of managing large model deployments. grape's policy-as-code model makes security configuration auditable, reproducible, and version-controllable. The system state is always derivable from the policy files. There are no hidden kernel parameters, no undocumented cgroup interactions, no emergent behaviors arising from the intersection of a dozen Linux subsystems each optimized for different workload classes.

ospab.os is not a general-purpose operating system that has been configured for AI inference. It is an AI inference substrate that happens to implement a general-purpose execution model for everything outside the inference hot path.

**7.3 Deployment Models and Integration Entry Points**

ospab.os supports three deployment architectures for partner integration:

**Bare metal.** ospab.os boots directly on physical inference hardware, with full access to IOMMU, NUMA topology, and hardware performance counters. This is the deployment model that realizes the full performance characteristics documented in Chapter 5.

**Type-1 hypervisor guest.** ospab.os runs as a guest on a Type-1 hypervisor (Xen, KVM with VFIO passthrough) with PCI passthrough of NPU and GPU devices. IOMMU protection is preserved through the passthrough configuration. Performance overhead relative to bare metal is bounded by the passthrough path's DMA remapping cost — typically under 3% for bulk tensor transfers.

**Confidential compute enclave.** ospab.os is compatible with AMD SEV-SNP and Intel TDX confidential VM architectures, enabling cryptographic isolation of the entire ospab.os memory space from the hypervisor layer. In this configuration, AETERNA's attestation mechanism extends to cover the confidential VM's measurement, enabling end-to-end verification from the physical TPM through the hypervisor boundary to the running ospab.os kernel and policy state.

The ospab partner SDK provides C and Rust bindings to the AETERNA IPC and capability APIs, enabling integration of existing inference frameworks — vLLM, TensorRT-LLM, and custom serving stacks — with ospab.os's SMR transport layer and pre-staging protocol. A reference integration for a standard transformer inference loop is included in the SDK distribution and documented in ospab's integration engineering guide.



────────────────────────────────────────────────────────────────────────────

*The future of machine intelligence requires an OS that speaks the language of tensors. That OS is ospab.os.*

────────────────────────────────────────────────────────────────────────────



*Confidential — ospab Strategic Architecture Division — v1.0.4 | February 2026*

