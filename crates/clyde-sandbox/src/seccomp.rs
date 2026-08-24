//! Seccomp filter construction.
//!
//! Phase 2a requires a seccomp filter on the namespace backend. The filter is
//! built here as raw classic-BPF instructions and handed to `bwrap` on a file
//! descriptor, which keeps the whole mechanism in safe Rust: no `unsafe` fd
//! manipulation, no `pre_exec` hook, and no additional dependency whose
//! serialisation we would have to trust.
//!
//! The policy is a **deny list over an allow-by-default base**, which is a
//! deliberate choice. An allow-list is the stronger shape, but a build sandbox
//! runs `cargo`, `rustc`, a linker, and arbitrary `build.rs` code, whose syscall
//! surface is wide and toolchain-dependent; an allow-list tight enough to be
//! worth having would break builds on the next toolchain bump, and a filter that
//! gets disabled to make builds work is worth nothing. The deny list closes the
//! specific escape and privilege-manipulation calls that namespace isolation
//! cares about, and the namespace and cgroup boundaries remain the primary
//! control.

/// A compiled filter, ready to write to the descriptor `bwrap --seccomp` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeccompFilter {
    instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Instruction {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

impl Instruction {
    fn to_bytes(self) -> [u8; 8] {
        let code = self.code.to_ne_bytes();
        let k = self.k.to_ne_bytes();
        [code[0], code[1], self.jt, self.jf, k[0], k[1], k[2], k[3]]
    }
}

// Classic BPF opcodes, as used by seccomp.
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JGE_K: u16 = 0x35;
const BPF_RET_K: u16 = 0x06;

// Offsets into `struct seccomp_data`.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;

// seccomp return actions.
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

// AUDIT_ARCH values.
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

/// The x32 ABI reuses x86_64 syscall numbers with this bit set. Anything in that
/// range is refused rather than filtered, because the numbers do not mean what
/// the deny list assumes.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Syscalls denied in every sandbox.
///
/// Each entry names why it is denied, because a bare number list rots.
struct DeniedCall {
    name: &'static str,
    x86_64: Option<u32>,
    aarch64: Option<u32>,
}

const DENIED: &[DeniedCall] = &[
    // Filesystem topology: a sandbox must not be able to change its own view.
    DeniedCall {
        name: "mount",
        x86_64: Some(165),
        aarch64: Some(40),
    },
    DeniedCall {
        name: "umount2",
        x86_64: Some(166),
        aarch64: Some(39),
    },
    DeniedCall {
        name: "pivot_root",
        x86_64: Some(155),
        aarch64: Some(41),
    },
    DeniedCall {
        name: "chroot",
        x86_64: Some(161),
        aarch64: None,
    },
    DeniedCall {
        name: "move_mount",
        x86_64: Some(429),
        aarch64: Some(429),
    },
    DeniedCall {
        name: "open_tree",
        x86_64: Some(428),
        aarch64: Some(428),
    },
    DeniedCall {
        name: "fsmount",
        x86_64: Some(432),
        aarch64: Some(432),
    },
    DeniedCall {
        name: "fsopen",
        x86_64: Some(430),
        aarch64: Some(430),
    },
    // Namespace manipulation: no nesting, no joining someone else's.
    DeniedCall {
        name: "setns",
        x86_64: Some(308),
        aarch64: Some(268),
    },
    DeniedCall {
        name: "unshare",
        x86_64: Some(272),
        aarch64: Some(97),
    },
    // Reaching into other processes.
    DeniedCall {
        name: "ptrace",
        x86_64: Some(101),
        aarch64: Some(117),
    },
    DeniedCall {
        name: "process_vm_readv",
        x86_64: Some(310),
        aarch64: Some(270),
    },
    DeniedCall {
        name: "process_vm_writev",
        x86_64: Some(311),
        aarch64: Some(271),
    },
    DeniedCall {
        name: "perf_event_open",
        x86_64: Some(298),
        aarch64: Some(241),
    },
    // Kernel modification.
    DeniedCall {
        name: "init_module",
        x86_64: Some(175),
        aarch64: Some(105),
    },
    DeniedCall {
        name: "finit_module",
        x86_64: Some(313),
        aarch64: Some(273),
    },
    DeniedCall {
        name: "delete_module",
        x86_64: Some(176),
        aarch64: Some(106),
    },
    DeniedCall {
        name: "kexec_load",
        x86_64: Some(246),
        aarch64: Some(104),
    },
    DeniedCall {
        name: "kexec_file_load",
        x86_64: Some(320),
        aarch64: Some(294),
    },
    DeniedCall {
        name: "bpf",
        x86_64: Some(321),
        aarch64: Some(280),
    },
    // Kernel keyring: a credential store the sandbox has no business touching.
    DeniedCall {
        name: "add_key",
        x86_64: Some(248),
        aarch64: Some(217),
    },
    DeniedCall {
        name: "request_key",
        x86_64: Some(249),
        aarch64: Some(218),
    },
    DeniedCall {
        name: "keyctl",
        x86_64: Some(250),
        aarch64: Some(219),
    },
    // Handle-based open bypasses path resolution, and therefore the mount table
    // that enforces edit scope.
    DeniedCall {
        name: "name_to_handle_at",
        x86_64: Some(303),
        aarch64: Some(264),
    },
    DeniedCall {
        name: "open_by_handle_at",
        x86_64: Some(304),
        aarch64: Some(265),
    },
    // Host state.
    DeniedCall {
        name: "reboot",
        x86_64: Some(169),
        aarch64: Some(142),
    },
    DeniedCall {
        name: "swapon",
        x86_64: Some(167),
        aarch64: Some(224),
    },
    DeniedCall {
        name: "swapoff",
        x86_64: Some(168),
        aarch64: Some(225),
    },
    DeniedCall {
        name: "settimeofday",
        x86_64: Some(164),
        aarch64: None,
    },
    DeniedCall {
        name: "clock_settime",
        x86_64: Some(227),
        aarch64: Some(112),
    },
    // userfaultfd has a long history of use in kernel exploitation and no
    // legitimate use in a build.
    DeniedCall {
        name: "userfaultfd",
        x86_64: Some(323),
        aarch64: Some(282),
    },
];

/// Which architecture a filter targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterArch {
    X86_64,
    Aarch64,
}

impl FilterArch {
    /// The architecture this binary is running on, or `None` where no filter is
    /// defined.
    ///
    /// Returning `None` rather than an empty filter matters: the caller refuses
    /// untrusted execution instead of running it unfiltered.
    pub fn host() -> Option<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(Self::X86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(Self::Aarch64)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            None
        }
    }

    fn audit_value(self) -> u32 {
        match self {
            Self::X86_64 => AUDIT_ARCH_X86_64,
            Self::Aarch64 => AUDIT_ARCH_AARCH64,
        }
    }

    fn denied_numbers(self) -> Vec<(&'static str, u32)> {
        DENIED
            .iter()
            .filter_map(|call| {
                let number = match self {
                    Self::X86_64 => call.x86_64,
                    Self::Aarch64 => call.aarch64,
                };
                number.map(|number| (call.name, number))
            })
            .collect()
    }
}

impl SeccompFilter {
    /// Builds the filter for `arch`.
    pub fn build(arch: FilterArch) -> Self {
        let denied = arch.denied_numbers();
        // Layout, with jumps resolved against the tail:
        //   ld  arch                     ; verify the architecture matches
        //   jne -> kill
        //   ld  nr
        //   jge X32_BIT -> kill          ; x86_64 only; harmless elsewhere
        //   jeq <denied> -> kill         ; one per denied call
        //   ret ALLOW
        //   ret KILL_PROCESS
        //
        // Every conditional jumps forward to the kill instruction, so the offset
        // is "distance to the end", computed from the remaining instruction
        // count rather than by patching afterwards.
        let checks = denied.len() + usize::from(matches!(arch, FilterArch::X86_64));
        let mut instructions = Vec::with_capacity(checks + 4);

        instructions.push(Instruction {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: OFFSET_ARCH,
        });
        // Remaining after this instruction: nr load + checks + ret ALLOW.
        let after_arch_check = 1 + checks + 1;
        instructions.push(Instruction {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: clamp_offset(after_arch_check),
            k: arch.audit_value(),
        });
        instructions.push(Instruction {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: OFFSET_NR,
        });

        let mut remaining = checks;
        if matches!(arch, FilterArch::X86_64) {
            remaining -= 1;
            instructions.push(Instruction {
                code: BPF_JMP_JGE_K,
                jt: clamp_offset(remaining + 1),
                jf: 0,
                k: X32_SYSCALL_BIT,
            });
        }
        for (_, number) in denied {
            remaining -= 1;
            instructions.push(Instruction {
                code: BPF_JMP_JEQ_K,
                jt: clamp_offset(remaining + 1),
                jf: 0,
                k: number,
            });
        }
        instructions.push(Instruction {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
        instructions.push(Instruction {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        });

        Self { instructions }
    }

    /// The instruction count. Filters must stay under the kernel's 4096 limit.
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// The raw bytes `bwrap --seccomp` expects: a packed array of
    /// `struct sock_filter` in native byte order.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.instructions
            .iter()
            .flat_map(|instruction| instruction.to_bytes())
            .collect()
    }

    /// The names of the syscalls this filter denies, for the audit record.
    pub fn denied_names(arch: FilterArch) -> Vec<&'static str> {
        arch.denied_numbers()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }
}

/// Classic BPF jump offsets are 8-bit. The filter is far below that bound, but
/// the clamp keeps the conversion total rather than panicking.
fn clamp_offset(offset: usize) -> u8 {
    u8::try_from(offset).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    /// A tiny interpreter for the subset of classic BPF this filter uses, so the
    /// tests assert on *behaviour* rather than on instruction encoding.
    fn evaluate(filter: &SeccompFilter, arch: u32, nr: u32) -> u32 {
        let mut pc = 0usize;
        let mut accumulator = 0u32;
        loop {
            let Some(instruction) = filter.instructions.get(pc) else {
                panic!("program counter ran off the end of the filter");
            };
            pc += 1;
            match instruction.code {
                BPF_LD_W_ABS => {
                    accumulator = match instruction.k {
                        OFFSET_NR => nr,
                        OFFSET_ARCH => arch,
                        other => panic!("unexpected seccomp_data offset {other}"),
                    };
                }
                BPF_JMP_JEQ_K => {
                    let taken = accumulator == instruction.k;
                    pc += usize::from(if taken {
                        instruction.jt
                    } else {
                        instruction.jf
                    });
                }
                BPF_JMP_JGE_K => {
                    let taken = accumulator >= instruction.k;
                    pc += usize::from(if taken {
                        instruction.jt
                    } else {
                        instruction.jf
                    });
                }
                BPF_RET_K => return instruction.k,
                other => panic!("unexpected opcode {other:#x}"),
            }
        }
    }

    #[test]
    fn ordinary_syscalls_are_allowed() {
        let filter = SeccompFilter::build(FilterArch::X86_64);
        // read, write, openat, execve, clone, futex.
        for nr in [0, 1, 257, 59, 56, 202] {
            assert_eq!(
                evaluate(&filter, AUDIT_ARCH_X86_64, nr),
                SECCOMP_RET_ALLOW,
                "syscall {nr} must be permitted; a build sandbox runs a compiler"
            );
        }
    }

    #[test]
    fn escape_and_privilege_syscalls_are_killed() {
        let filter = SeccompFilter::build(FilterArch::X86_64);
        let cases = [
            ("mount", 165u32),
            ("pivot_root", 155),
            ("ptrace", 101),
            ("setns", 308),
            ("unshare", 272),
            ("bpf", 321),
            ("keyctl", 250),
            ("open_by_handle_at", 304),
            ("userfaultfd", 323),
            ("init_module", 175),
        ];
        for (name, nr) in cases {
            assert_eq!(
                evaluate(&filter, AUDIT_ARCH_X86_64, nr),
                SECCOMP_RET_KILL_PROCESS,
                "{name} must be denied"
            );
        }
    }

    #[test]
    fn a_mismatched_architecture_is_killed() {
        let filter = SeccompFilter::build(FilterArch::X86_64);
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_AARCH64, 0),
            SECCOMP_RET_KILL_PROCESS,
            "a syscall from an unexpected architecture must not be evaluated against x86_64 numbers"
        );
    }

    #[test]
    fn the_x32_abi_is_refused_wholesale() {
        let filter = SeccompFilter::build(FilterArch::X86_64);
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_X86_64, X32_SYSCALL_BIT),
            SECCOMP_RET_KILL_PROCESS,
            "x32 numbers do not mean what the deny list assumes"
        );
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_X86_64, X32_SYSCALL_BIT + 165),
            SECCOMP_RET_KILL_PROCESS
        );
    }

    #[test]
    fn the_aarch64_filter_denies_its_own_numbers() {
        let filter = SeccompFilter::build(FilterArch::Aarch64);
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_AARCH64, 40),
            SECCOMP_RET_KILL_PROCESS,
            "mount on aarch64"
        );
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_AARCH64, 63),
            SECCOMP_RET_ALLOW,
            "read on aarch64"
        );
        // The x86_64 number for mount must not be denied on aarch64, because it
        // means something else there.
        assert_eq!(
            evaluate(&filter, AUDIT_ARCH_AARCH64, 165),
            SECCOMP_RET_ALLOW
        );
    }

    #[test]
    fn the_encoding_is_eight_bytes_per_instruction_and_within_the_kernel_limit() {
        let filter = SeccompFilter::build(FilterArch::X86_64);
        assert_eq!(filter.to_bytes().len(), filter.len() * 8);
        assert!(
            filter.len() < 4096,
            "the kernel rejects filters over 4096 instructions"
        );
        assert!(!filter.is_empty());
    }

    #[test]
    fn denied_names_are_reportable() {
        let names = SeccompFilter::denied_names(FilterArch::X86_64);
        assert!(names.contains(&"mount"));
        assert!(names.contains(&"ptrace"));
        assert!(!names.is_empty());
    }
}
