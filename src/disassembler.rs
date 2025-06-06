#![allow(clippy::arithmetic_side_effects)]
// Copyright 2017 6WIND S.A. <quentin.monnet@6wind.com>
//
// Licensed under the Apache License, Version 2.0 <http://www.apache.org/licenses/LICENSE-2.0> or
// the MIT license <http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Functions in this module are used to handle eBPF programs with a higher level representation,
//! for example to disassemble the code into a human-readable format.

use alloc::{format, string::{ToString, String}};

use crate::{
    ebpf,
    program::{BuiltinProgram, FunctionRegistry, SBPFVersion},
    static_analysis::CfgNode,
    vm::ContextObject,
};
use alloc::collections::BTreeMap;

fn resolve_label(cfg_nodes: &BTreeMap<usize, CfgNode>, pc: usize) -> &str {
    cfg_nodes
        .get(&pc)
        .map(|cfg_node| cfg_node.label.as_str())
        .unwrap_or("[invalid]")
}

#[inline]
fn alu_imm_str(name: &str, insn: &ebpf::Insn) -> String {
    format!("{} r{}, {}", name, insn.dst, insn.imm)
}

#[inline]
fn alu_reg_str(name: &str, insn: &ebpf::Insn) -> String {
    format!("{} r{}, r{}", name, insn.dst, insn.src)
}

#[inline]
fn byteswap_str(name: &str, insn: &ebpf::Insn) -> String {
    match insn.imm {
        16 | 32 | 64 => {}
        _ => jam_pvm_common::info!("[Disassembler] Warning: Invalid offset value for {name} insn"),
    }
    format!("{}{} r{}", name, insn.imm, insn.dst)
}

#[inline]
fn signed_off_str(value: i16) -> String {
    if value < 0 {
        format!("-{:#x}", -(value as isize))
    } else {
        format!("+{value:#x}")
    }
}

#[inline]
fn ld_st_imm_str(name: &str, insn: &ebpf::Insn) -> String {
    format!(
        "{} [r{}{}], {}",
        name,
        insn.dst,
        signed_off_str(insn.off),
        insn.imm
    )
}

#[inline]
fn ld_reg_str(name: &str, insn: &ebpf::Insn) -> String {
    format!(
        "{} r{}, [r{}{}]",
        name,
        insn.dst,
        insn.src,
        signed_off_str(insn.off)
    )
}

#[inline]
fn st_reg_str(name: &str, insn: &ebpf::Insn) -> String {
    format!(
        "{} [r{}{}], r{}",
        name,
        insn.dst,
        signed_off_str(insn.off),
        insn.src
    )
}

#[inline]
fn jmp_imm_str(name: &str, insn: &ebpf::Insn, cfg_nodes: &BTreeMap<usize, CfgNode>) -> String {
    let target_pc = (insn.ptr as isize + insn.off as isize + 1) as usize;
    format!(
        "{} r{}, {}, {}",
        name,
        insn.dst,
        insn.imm,
        resolve_label(cfg_nodes, target_pc)
    )
}

#[inline]
fn jmp_reg_str(name: &str, insn: &ebpf::Insn, cfg_nodes: &BTreeMap<usize, CfgNode>) -> String {
    let target_pc = (insn.ptr as isize + insn.off as isize + 1) as usize;
    format!(
        "{} r{}, r{}, {}",
        name,
        insn.dst,
        insn.src,
        resolve_label(cfg_nodes, target_pc)
    )
}

/// Disassemble an eBPF instruction
#[rustfmt::skip]
pub fn disassemble_instruction<C: ContextObject>(
    insn: &ebpf::Insn,
    pc: usize,
    cfg_nodes: &BTreeMap<usize, CfgNode>,
    function_registry: &FunctionRegistry<usize>,
    loader: &BuiltinProgram<C>,
    sbpf_version: SBPFVersion,
) -> String {
    let name;
    let desc;
    match insn.opc {
        // BPF_LD class
        ebpf::LD_DW_IMM  => { name = "lddw"; desc = format!("{} r{:}, {:#x}", name, insn.dst, insn.imm); },

        // BPF_LDX class
        ebpf::LD_B_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "ldxb";  desc = ld_reg_str(name, insn); },
        ebpf::LD_H_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "ldxh";  desc = ld_reg_str(name, insn); },
        ebpf::LD_W_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "ldxw";  desc = ld_reg_str(name, insn); },
        ebpf::LD_DW_REG if !sbpf_version.move_memory_instruction_classes() => { name = "ldxdw"; desc = ld_reg_str(name, insn); },

        // BPF_ST class
        ebpf::ST_B_IMM  if !sbpf_version.move_memory_instruction_classes() => { name = "stb";   desc = ld_st_imm_str(name, insn); },
        ebpf::ST_H_IMM  if !sbpf_version.move_memory_instruction_classes() => { name = "sth";   desc = ld_st_imm_str(name, insn); },
        ebpf::ST_W_IMM  if !sbpf_version.move_memory_instruction_classes() => { name = "stw";   desc = ld_st_imm_str(name, insn); },
        ebpf::ST_DW_IMM if !sbpf_version.move_memory_instruction_classes() => { name = "stdw";  desc = ld_st_imm_str(name, insn); },

        // BPF_STX class
        ebpf::ST_B_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "stxb";  desc = st_reg_str(name, insn); },
        ebpf::ST_H_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "stxh";  desc = st_reg_str(name, insn); },
        ebpf::ST_W_REG  if !sbpf_version.move_memory_instruction_classes() => { name = "stxw";  desc = st_reg_str(name, insn); },
        ebpf::ST_DW_REG if !sbpf_version.move_memory_instruction_classes() => { name = "stxdw"; desc = st_reg_str(name, insn); },

        // BPF_ALU32_LOAD class
        ebpf::ADD32_IMM  => { name = "add32";  desc = alu_imm_str(name, insn);  },
        ebpf::ADD32_REG  => { name = "add32";  desc = alu_reg_str(name, insn);  },
        ebpf::SUB32_IMM  => { name = "sub32";  desc = alu_imm_str(name, insn);  },
        ebpf::SUB32_REG  => { name = "sub32";  desc = alu_reg_str(name, insn);  },
        ebpf::MUL32_IMM  if !sbpf_version.enable_pqr() => { name = "mul32";  desc = alu_imm_str(name, insn);  },
        ebpf::MUL32_REG  if !sbpf_version.enable_pqr() => { name = "mul32";  desc = alu_reg_str(name, insn);  },
        ebpf::LD_1B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "ldxb";  desc = ld_reg_str(name, insn); },
        ebpf::DIV32_IMM  if !sbpf_version.enable_pqr() => { name = "div32";  desc = alu_imm_str(name, insn);  },
        ebpf::DIV32_REG  if !sbpf_version.enable_pqr() => { name = "div32";  desc = alu_reg_str(name, insn);  },
        ebpf::LD_2B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "ldxh";  desc = ld_reg_str(name, insn); },
        ebpf::OR32_IMM   => { name = "or32";   desc = alu_imm_str(name, insn);  },
        ebpf::OR32_REG   => { name = "or32";   desc = alu_reg_str(name, insn);  },
        ebpf::AND32_IMM  => { name = "and32";  desc = alu_imm_str(name, insn);  },
        ebpf::AND32_REG  => { name = "and32";  desc = alu_reg_str(name, insn);  },
        ebpf::LSH32_IMM  => { name = "lsh32";  desc = alu_imm_str(name, insn);  },
        ebpf::LSH32_REG  => { name = "lsh32";  desc = alu_reg_str(name, insn);  },
        ebpf::RSH32_IMM  => { name = "rsh32";  desc = alu_imm_str(name, insn);  },
        ebpf::RSH32_REG  => { name = "rsh32";  desc = alu_reg_str(name, insn);  },
        ebpf::NEG32      if !sbpf_version.disable_neg() => { name = "neg32";  desc = format!("{} r{}", name, insn.dst); },
        ebpf::LD_4B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "ldxw";  desc = ld_reg_str(name, insn); },
        ebpf::MOD32_IMM  if !sbpf_version.enable_pqr() => { name = "mod32";  desc = alu_imm_str(name, insn);  },
        ebpf::MOD32_REG  if !sbpf_version.enable_pqr() => { name = "mod32";  desc = alu_reg_str(name, insn);  },
        ebpf::LD_8B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "ldxdw"; desc = ld_reg_str(name, insn); },
        ebpf::XOR32_IMM  => { name = "xor32";  desc = alu_imm_str(name, insn);  },
        ebpf::XOR32_REG  => { name = "xor32";  desc = alu_reg_str(name, insn);  },
        ebpf::MOV32_IMM  => { name = "mov32";  desc = alu_imm_str(name, insn);  },
        ebpf::MOV32_REG  => { name = "mov32";  desc = alu_reg_str(name, insn);  },
        ebpf::ARSH32_IMM => { name = "arsh32"; desc = alu_imm_str(name, insn);  },
        ebpf::ARSH32_REG => { name = "arsh32"; desc = alu_reg_str(name, insn);  },
        ebpf::LE         => { name = "le";     desc = byteswap_str(name, insn); },
        ebpf::BE         => { name = "be";     desc = byteswap_str(name, insn); },

        // BPF_ALU64_STORE class
        ebpf::ADD64_IMM  => { name = "add64";  desc = alu_imm_str(name, insn); },
        ebpf::ADD64_REG  => { name = "add64";  desc = alu_reg_str(name, insn); },
        ebpf::SUB64_IMM  => { name = "sub64";  desc = alu_imm_str(name, insn); },
        ebpf::SUB64_REG  => { name = "sub64";  desc = alu_reg_str(name, insn); },
        ebpf::MUL64_IMM  if !sbpf_version.enable_pqr() => { name = "mul64";  desc = alu_imm_str(name, insn); },
        ebpf::ST_1B_IMM  if sbpf_version.move_memory_instruction_classes() => { name = "stb";   desc = ld_st_imm_str(name, insn); },
        ebpf::MUL64_REG  if !sbpf_version.enable_pqr() => { name = "mul64";  desc = alu_reg_str(name, insn); },
        ebpf::ST_1B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "stxb";  desc = st_reg_str(name, insn); },
        ebpf::DIV64_IMM  if !sbpf_version.enable_pqr() => { name = "div64";  desc = alu_imm_str(name, insn); },
        ebpf::ST_2B_IMM  if sbpf_version.move_memory_instruction_classes() => { name = "sth";   desc = ld_st_imm_str(name, insn); },
        ebpf::DIV64_REG  if !sbpf_version.enable_pqr() => { name = "div64";  desc = alu_reg_str(name, insn); },
        ebpf::ST_2B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "stxh";  desc = st_reg_str(name, insn); },
        ebpf::OR64_IMM   => { name = "or64";   desc = alu_imm_str(name, insn); },
        ebpf::OR64_REG   => { name = "or64";   desc = alu_reg_str(name, insn); },
        ebpf::AND64_IMM  => { name = "and64";  desc = alu_imm_str(name, insn); },
        ebpf::AND64_REG  => { name = "and64";  desc = alu_reg_str(name, insn); },
        ebpf::LSH64_IMM  => { name = "lsh64";  desc = alu_imm_str(name, insn); },
        ebpf::LSH64_REG  => { name = "lsh64";  desc = alu_reg_str(name, insn); },
        ebpf::RSH64_IMM  => { name = "rsh64";  desc = alu_imm_str(name, insn); },
        ebpf::RSH64_REG  => { name = "rsh64";  desc = alu_reg_str(name, insn); },
        ebpf::ST_4B_IMM  if sbpf_version.move_memory_instruction_classes() => { name = "stw";   desc = ld_st_imm_str(name, insn); },
        ebpf::NEG64      if !sbpf_version.disable_neg() => { name = "neg64";  desc = format!("{} r{}", name, insn.dst); },
        ebpf::ST_4B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "stxw";  desc = st_reg_str(name, insn); },
        ebpf::MOD64_IMM  if !sbpf_version.enable_pqr() => { name = "mod64";  desc = alu_imm_str(name, insn); },
        ebpf::ST_8B_IMM  if sbpf_version.move_memory_instruction_classes() => { name = "stdw";  desc = ld_st_imm_str(name, insn); },
        ebpf::MOD64_REG  if !sbpf_version.enable_pqr() => { name = "mod64";  desc = alu_reg_str(name, insn); },
        ebpf::ST_8B_REG  if sbpf_version.move_memory_instruction_classes() => { name = "stxdw"; desc = st_reg_str(name, insn); },
        ebpf::XOR64_IMM  => { name = "xor64";  desc = alu_imm_str(name, insn); },
        ebpf::XOR64_REG  => { name = "xor64";  desc = alu_reg_str(name, insn); },
        ebpf::MOV64_IMM  => { name = "mov64";  desc = alu_imm_str(name, insn); },
        ebpf::MOV64_REG  => { name = "mov64";  desc = alu_reg_str(name, insn); },
        ebpf::ARSH64_IMM => { name = "arsh64"; desc = alu_imm_str(name, insn); },
        ebpf::ARSH64_REG => { name = "arsh64"; desc = alu_reg_str(name, insn); },
        ebpf::HOR64_IMM  => { name = "hor64"; desc = alu_imm_str(name, insn); },

        // BPF_PQR class
        ebpf::LMUL32_IMM  if sbpf_version.enable_pqr() => { name = "lmul32"; desc = alu_imm_str(name, insn); },
        ebpf::LMUL32_REG  if sbpf_version.enable_pqr() => { name = "lmul32"; desc = alu_reg_str(name, insn); },
        ebpf::LMUL64_IMM  if sbpf_version.enable_pqr() => { name = "lmul64"; desc = alu_imm_str(name, insn); },
        ebpf::LMUL64_REG  if sbpf_version.enable_pqr() => { name = "lmul64"; desc = alu_reg_str(name, insn); },
        ebpf::UHMUL64_IMM if sbpf_version.enable_pqr() => { name = "uhmul64"; desc = alu_imm_str(name, insn); },
        ebpf::UHMUL64_REG if sbpf_version.enable_pqr() => { name = "uhmul64"; desc = alu_reg_str(name, insn); },
        ebpf::SHMUL64_IMM if sbpf_version.enable_pqr() => { name = "shmul64"; desc = alu_imm_str(name, insn); },
        ebpf::SHMUL64_REG if sbpf_version.enable_pqr() => { name = "shmul64"; desc = alu_reg_str(name, insn); },
        ebpf::UDIV32_IMM  if sbpf_version.enable_pqr() => { name = "udiv32"; desc = alu_imm_str(name, insn); },
        ebpf::UDIV32_REG  if sbpf_version.enable_pqr() => { name = "udiv32"; desc = alu_reg_str(name, insn); },
        ebpf::UDIV64_IMM  if sbpf_version.enable_pqr() => { name = "udiv64"; desc = alu_imm_str(name, insn); },
        ebpf::UDIV64_REG  if sbpf_version.enable_pqr() => { name = "udiv64"; desc = alu_reg_str(name, insn); },
        ebpf::UREM32_IMM  if sbpf_version.enable_pqr() => { name = "urem32"; desc = alu_imm_str(name, insn); },
        ebpf::UREM32_REG  if sbpf_version.enable_pqr() => { name = "urem32"; desc = alu_reg_str(name, insn); },
        ebpf::UREM64_IMM  if sbpf_version.enable_pqr() => { name = "urem64"; desc = alu_imm_str(name, insn); },
        ebpf::UREM64_REG  if sbpf_version.enable_pqr() => { name = "urem64"; desc = alu_reg_str(name, insn); },
        ebpf::SDIV32_IMM  if sbpf_version.enable_pqr() => { name = "sdiv32"; desc = alu_imm_str(name, insn); },
        ebpf::SDIV32_REG  if sbpf_version.enable_pqr() => { name = "sdiv32"; desc = alu_reg_str(name, insn); },
        ebpf::SDIV64_IMM  if sbpf_version.enable_pqr() => { name = "sdiv64"; desc = alu_imm_str(name, insn); },
        ebpf::SDIV64_REG  if sbpf_version.enable_pqr() => { name = "sdiv64"; desc = alu_reg_str(name, insn); },
        ebpf::SREM32_IMM  if sbpf_version.enable_pqr() => { name = "srem32"; desc = alu_imm_str(name, insn); },
        ebpf::SREM32_REG  if sbpf_version.enable_pqr() => { name = "srem32"; desc = alu_reg_str(name, insn); },
        ebpf::SREM64_IMM  if sbpf_version.enable_pqr() => { name = "srem64"; desc = alu_imm_str(name, insn); },
        ebpf::SREM64_REG  if sbpf_version.enable_pqr() => { name = "srem64"; desc = alu_reg_str(name, insn); },

        // BPF_JMP class
        ebpf::JA         => {
            name = "ja";
            let target_pc = (insn.ptr as isize + insn.off as isize + 1) as usize;
            desc = format!("{} {}", name, resolve_label(cfg_nodes, target_pc));
        },
        ebpf::JEQ_IMM    => { name = "jeq";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JEQ_REG    => { name = "jeq";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JGT_IMM    => { name = "jgt";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JGT_REG    => { name = "jgt";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JGE_IMM    => { name = "jge";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JGE_REG    => { name = "jge";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JLT_IMM    => { name = "jlt";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JLT_REG    => { name = "jlt";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JLE_IMM    => { name = "jle";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JLE_REG    => { name = "jle";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JSET_IMM   => { name = "jset"; desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JSET_REG   => { name = "jset"; desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JNE_IMM    => { name = "jne";  desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JNE_REG    => { name = "jne";  desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JSGT_IMM   => { name = "jsgt"; desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JSGT_REG   => { name = "jsgt"; desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JSGE_IMM   => { name = "jsge"; desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JSGE_REG   => { name = "jsge"; desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JSLT_IMM   => { name = "jslt"; desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JSLT_REG   => { name = "jslt"; desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::JSLE_IMM   => { name = "jsle"; desc = jmp_imm_str(name, insn, cfg_nodes); },
        ebpf::JSLE_REG   => { name = "jsle"; desc = jmp_reg_str(name, insn, cfg_nodes); },
        ebpf::CALL_IMM   => {
            let key = sbpf_version.calculate_call_imm_target_pc(pc, insn.imm);
            let mut name = "call";
            let mut function_name = function_registry.lookup_by_key(key).map(|(function_name, _)| String::from_utf8_lossy(function_name).to_string());
            if !sbpf_version.static_syscalls() && function_name.is_none() {
                name = "syscall";
                function_name = loader.get_function_registry().lookup_by_key(insn.imm as u32).map(|(function_name, _)| String::from_utf8_lossy(function_name).to_string());
            }
            desc = format!("{} {}", name, function_name.unwrap_or_else(|| "[invalid]".to_string()));
        },
        ebpf::CALL_REG   => { name = "callx"; desc = format!("{} r{}", name, if sbpf_version.callx_uses_src_reg() { insn.src } else { insn.imm as u8 }); },
        ebpf::EXIT     if !sbpf_version.static_syscalls() => { name = "exit"; desc = name.to_string(); },
        ebpf::RETURN   if sbpf_version.static_syscalls() =>  { name = "return"; desc = name.to_string(); },
        ebpf::SYSCALL  if sbpf_version.static_syscalls() =>  { desc = format!("syscall {}", insn.imm); },

        _                => { name = "unknown"; desc = format!("{} opcode={:#x}", name, insn.opc); },
    };
    desc
}
