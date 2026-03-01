/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Compute-First Scheduler for AETERNA microkernel (tech-manifest п.10).
Phase 2: Basic task queue with priority support.
Full preemptive scheduling will come in Phase 3.
*/

/// Task state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Task is ready to run
    Ready,
    /// Task is currently running
    Running,
    /// Task is blocked (waiting for I/O, IPC, etc.)
    Blocked,
    /// Task has terminated
    Dead,
}

/// Task priority levels (Compute-First: AI workloads get highest priority)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    /// Idle tasks (background maintenance)
    Idle = 0,
    /// Normal user tasks
    Normal = 1,
    /// System services (drivers, IPC servers)
    System = 2,
    /// Real-time tasks (interactive, latency-sensitive)
    RealTime = 3,
    /// Compute-critical (AI inference, tensor operations)
    /// Gets up to 99% of CPU quanta per tech-manifest п.10
    Compute = 4,
}

/// Unique task identifier
pub type TaskId = u64;

/// Task Control Block (TCB)
#[derive(Debug, Clone, Copy)]
pub struct Task {
    /// Unique task ID
    pub id: TaskId,
    /// Current state
    pub state: TaskState,
    /// Priority level
    pub priority: Priority,
    /// Saved stack pointer (for context switching)
    pub stack_pointer: u64,
    /// Saved instruction pointer
    pub instruction_pointer: u64,
    /// CPU time consumed (in timer ticks)
    pub cpu_ticks: u64,
    /// Whether this task is non-preemptive (tech-manifest п.10)
    pub non_preemptive: bool,
}

/// Maximum number of tasks (early boot limit, will grow with heap)
const MAX_TASKS: usize = 64;

static mut TASKS: [Option<Task>; MAX_TASKS] = [None; MAX_TASKS];
static mut CURRENT_TASK: usize = 0;
static mut NEXT_TASK_ID: TaskId = 1;
static mut SCHEDULER_INITIALIZED: bool = false;

/// Initialize the scheduler
pub fn init() {
    unsafe {
        // Create the kernel idle task (task 0)
        TASKS[0] = Some(Task {
            id: 0,
            state: TaskState::Running,
            priority: Priority::Idle,
            stack_pointer: 0,
            instruction_pointer: 0,
            cpu_ticks: 0,
            non_preemptive: false,
        });
        CURRENT_TASK = 0;
        NEXT_TASK_ID = 1;
        SCHEDULER_INITIALIZED = true;
    }

    crate::arch::x86_64::serial::write_str("[AETERNA] Scheduler initialized (Compute-First)\r\n");
}

/// Create a new task (stub  no actual context switching yet)
pub fn create_task(priority: Priority, entry_point: u64, stack_pointer: u64) -> Option<TaskId> {
    unsafe {
        if !SCHEDULER_INITIALIZED {
            return None;
        }

        // Find a free slot
        for i in 1..MAX_TASKS {
            if TASKS[i].is_none() {
                let id = NEXT_TASK_ID;
                NEXT_TASK_ID += 1;

                TASKS[i] = Some(Task {
                    id,
                    state: TaskState::Ready,
                    priority,
                    stack_pointer,
                    instruction_pointer: entry_point,
                    cpu_ticks: 0,
                    non_preemptive: priority == Priority::Compute,
                });

                return Some(id);
            }
        }
        None // No free slots
    }
}

/// Get the currently running task ID
pub fn current_task_id() -> TaskId {
    unsafe {
        if let Some(ref task) = TASKS[CURRENT_TASK] {
            task.id
        } else {
            0
        }
    }
}

/// Get number of active tasks
pub fn task_count() -> usize {
    unsafe {
        TASKS.iter().filter(|t| t.is_some()).count()
    }
}

/// Pick the next task to run (priority-based, highest priority first)
/// This is the core scheduling decision  Compute-First means
/// Priority::Compute tasks always run before anything else.
pub fn schedule_next() -> Option<usize> {
    unsafe {
        if !SCHEDULER_INITIALIZED {
            return None;
        }

        let mut best_slot: Option<usize> = None;
        let mut best_priority = Priority::Idle;

        for i in 0..MAX_TASKS {
            if let Some(ref task) = TASKS[i] {
                if task.state == TaskState::Ready && task.priority >= best_priority {
                    best_priority = task.priority;
                    best_slot = Some(i);
                }
            }
        }

        best_slot
    }
}

/// Called on each timer tick to update scheduler state
pub fn tick() {
    unsafe {
        if !SCHEDULER_INITIALIZED {
            return;
        }

        // Increment CPU time for current task
        if let Some(ref mut task) = TASKS[CURRENT_TASK] {
            task.cpu_ticks += 1;
        }

        // In Phase 2, we don't actually preempt  just track time.
        // Full preemption comes with Phase 3 context switching.
    }
}

/// Check if scheduler is initialized
pub fn is_initialized() -> bool {
    unsafe { SCHEDULER_INITIALIZED }
}