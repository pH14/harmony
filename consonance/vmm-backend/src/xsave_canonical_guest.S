/* SPDX-License-Identifier: AGPL-3.0-or-later */
.text
.balign 16
.global harmony_xsave_canonical_start
.global harmony_xsave_canonical_standard
.global harmony_xsave_canonical_compacted
.global harmony_xsave_canonical_saved
.global harmony_xsave_canonical_padding
.global harmony_xsave_canonical_end
.hidden harmony_xsave_canonical_start
.hidden harmony_xsave_canonical_standard
.hidden harmony_xsave_canonical_compacted
.hidden harmony_xsave_canonical_saved
.hidden harmony_xsave_canonical_padding
.hidden harmony_xsave_canonical_end
harmony_xsave_canonical_start:
    cld
    mov rdi, 0x2000
    mov rcx, 104
    mov rax, 0xa5a5a5a5a5a5a5a5
    rep stosq
    mov eax, 7
    xor edx, edx
    xrstor64 [0x3000]
    cmp r12d, 1
    je .Lsse
    cmp r12d, 2
    je .Lavx
    cmp r12d, 3
    jne .Lsave
    ldmxcsr [0x301c]
    jmp .Lsave
.Lsse:
    pcmpeqd xmm0, xmm0
    jmp .Lsave
.Lavx:
    vpcmpeqd ymm0, ymm0, ymm0
.Lsave:
    test r14d, r14d
    jnz harmony_xsave_canonical_compacted
harmony_xsave_canonical_standard:
    xsave64 [0x2000]
    jmp harmony_xsave_canonical_saved
harmony_xsave_canonical_compacted:
    xsavec64 [0x2000]
harmony_xsave_canonical_saved:
    test r13d, r13d
    jz .Ldone
    mov rbx, 0x2000
    mov r8, [rbx + 512]
    and r8d, 7
    test r8b, 1
    jnz .Lx87_present
    xor eax, eax
    mov rdi, rbx
    mov ecx, 3
    rep stosq
    mov word ptr [rbx], 0x37f
    lea rdi, [rbx + 32]
    mov ecx, 16
    rep stosq
.Lx87_present:
    mov byte ptr [rbx + 5], 0
    and word ptr [rbx + 6], 0x7ff
    lea rdi, [rbx + 42]
    mov ecx, 8
.Lst_padding:
    mov dword ptr [rdi], 0
    mov word ptr [rdi + 4], 0
    add rdi, 16
    loop .Lst_padding
    cmp word ptr [rbx], 0x37f
    jne .Lsse_component
    cmp word ptr [rbx + 2], 0
    jne .Lsse_component
    cmp dword ptr [rbx + 4], 0
    jne .Lsse_component
    cmp qword ptr [rbx + 8], 0
    jne .Lsse_component
    cmp qword ptr [rbx + 16], 0
    jne .Lsse_component
    lea rdi, [rbx + 32]
    mov ecx, 16
    xor eax, eax
    repe scasq
    jne .Lsse_component
    and r8d, -2
.Lsse_component:
    test r8b, 2
    jnz .Lsse_compare
    lea rdi, [rbx + 160]
    mov ecx, 32
    xor eax, eax
    rep stosq
.Lsse_compare:
    lea rdi, [rbx + 160]
    mov ecx, 32
    xor eax, eax
    repe scasq
    jne .Lavx_component
    and r8d, -3
.Lavx_component:
    test r8b, 4
    jnz .Lavx_compare
    lea rdi, [rbx + 576]
    mov ecx, 32
    xor eax, eax
    rep stosq
.Lavx_compare:
    lea rdi, [rbx + 576]
    mov ecx, 32
    xor eax, eax
    repe scasq
    jne harmony_xsave_canonical_padding
    and r8d, -5
harmony_xsave_canonical_padding:
    stmxcsr [rbx + 24]
    mov dword ptr [rbx + 28], 0xffff
    lea rdi, [rbx + 416]
    mov ecx, 12
    xor eax, eax
    rep stosq
    mov [rbx + 512], r8
    xor eax, eax
    test r14d, r14d
    jz .Lformat
    mov rax, 0x8000000000000007
.Lformat:
    mov [rbx + 520], rax
    lea rdi, [rbx + 528]
    mov ecx, 6
    xor eax, eax
    rep stosq
.Ldone:
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    xor edi, edi
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    xor eax, eax
    hlt
harmony_xsave_canonical_end:
