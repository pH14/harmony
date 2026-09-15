
objects/dl-reloc.o:     file format elf64-x86-64


Disassembly of section .text:

0000000000000000 <_dl_try_allocate_static_tls>:
       0:	f3 0f 1e fa          	endbr64
       4:	48 83 bf 88 04 00 00 ff 	cmpq   $0xffffffffffffffff,0x488(%rdi)
       c:	0f 84 ce 00 00 00    	je     e0 <_dl_try_allocate_static_tls+0xe0>
      12:	4c 8b 8f 78 04 00 00 	mov    0x478(%rdi),%r9
      19:	4c 39 0d 00 00 00 00 	cmp    %r9,0x0(%rip)        # 20 <_dl_try_allocate_static_tls+0x20>	1c: R_X86_64_PC32	_dl_tls_static_align-0x4
      20:	0f 82 ba 00 00 00    	jb     e0 <_dl_try_allocate_static_tls+0xe0>
      26:	4c 8b 05 00 00 00 00 	mov    0x0(%rip),%r8        # 2d <_dl_try_allocate_static_tls+0x2d>	29: R_X86_64_PC32	_dl_tls_static_used-0x4
      2d:	48 8b 0d 00 00 00 00 	mov    0x0(%rip),%rcx        # 34 <_dl_try_allocate_static_tls+0x34>	30: R_X86_64_PC32	_dl_tls_static_size-0x4
      34:	4c 29 c1             	sub    %r8,%rcx
      37:	48 81 f9 3f 09 00 00 	cmp    $0x93f,%rcx
      3e:	0f 86 9c 00 00 00    	jbe    e0 <_dl_try_allocate_static_tls+0xe0>
      44:	4c 8b 97 80 04 00 00 	mov    0x480(%rdi),%r10
      4b:	48 8b 87 70 04 00 00 	mov    0x470(%rdi),%rax
      52:	48 81 e9 40 09 00 00 	sub    $0x940,%rcx
      59:	4c 01 d0             	add    %r10,%rax
      5c:	48 39 c1             	cmp    %rax,%rcx
      5f:	72 7f                	jb     e0 <_dl_try_allocate_static_tls+0xe0>
      61:	41 89 f3             	mov    %esi,%r11d
      64:	48 89 ce             	mov    %rcx,%rsi
      67:	31 d2                	xor    %edx,%edx
      69:	48 29 c6             	sub    %rax,%rsi
      6c:	48 89 f0             	mov    %rsi,%rax
      6f:	49 f7 f1             	div    %r9
      72:	48 89 c8             	mov    %rcx,%rax
      75:	4c 29 d0             	sub    %r10,%rax
      78:	48 29 d6             	sub    %rdx,%rsi
      7b:	48 29 f0             	sub    %rsi,%rax
      7e:	45 84 db             	test   %r11b,%r11b
      81:	74 1c                	je     9f <_dl_try_allocate_static_tls+0x9f>
      83:	48 8b 15 00 00 00 00 	mov    0x0(%rip),%rdx        # 8a <_dl_try_allocate_static_tls+0x8a>	86: R_X86_64_PC32	_dl_tls_static_optional-0x4
      8a:	48 39 c2             	cmp    %rax,%rdx
      8d:	72 51                	jb     e0 <_dl_try_allocate_static_tls+0xe0>
      8f:	48 29 ca             	sub    %rcx,%rdx
      92:	4c 01 d2             	add    %r10,%rdx
      95:	48 01 f2             	add    %rsi,%rdx
      98:	48 89 15 00 00 00 00 	mov    %rdx,0x0(%rip)        # 9f <_dl_try_allocate_static_tls+0x9f>	9b: R_X86_64_PC32	_dl_tls_static_optional-0x4
      9f:	4c 01 c0             	add    %r8,%rax
      a2:	48 89 05 00 00 00 00 	mov    %rax,0x0(%rip)        # a9 <_dl_try_allocate_static_tls+0xa9>	a5: R_X86_64_PC32	_dl_tls_static_used-0x4
      a9:	48 89 87 88 04 00 00 	mov    %rax,0x488(%rdi)
      b0:	48 8b 47 28          	mov    0x28(%rdi),%rax
      b4:	f6 80 54 03 00 00 08 	testb  $0x8,0x354(%rax)
      bb:	75 13                	jne    d0 <_dl_try_allocate_static_tls+0xd0>
      bd:	80 8f 55 03 00 00 80 	orb    $0x80,0x355(%rdi)
      c4:	31 c0                	xor    %eax,%eax
      c6:	c3                   	ret
      c7:	66 0f 1f 84 00 00 00 00 00 	nopw   0x0(%rax,%rax,1)
      d0:	55                   	push   %rbp
      d1:	48 89 e5             	mov    %rsp,%rbp
      d4:	e8 00 00 00 00       	call   d9 <_dl_try_allocate_static_tls+0xd9>	d5: R_X86_64_PLT32	_dl_init_static_tls-0x4
      d9:	31 c0                	xor    %eax,%eax
      db:	5d                   	pop    %rbp
      dc:	c3                   	ret
      dd:	0f 1f 00             	nopl   (%rax)
      e0:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
      e5:	c3                   	ret
      e6:	66 2e 0f 1f 84 00 00 00 00 00 	cs nopw 0x0(%rax,%rax,1)

00000000000000f0 <_dl_allocate_static_tls>:
      f0:	f3 0f 1e fa          	endbr64
      f4:	48 83 bf 88 04 00 00 ff 	cmpq   $0xffffffffffffffff,0x488(%rdi)
      fc:	0f 84 93 00 00 00    	je     195 <_dl_allocate_static_tls+0xa5>
     102:	4c 8b 87 78 04 00 00 	mov    0x478(%rdi),%r8
     109:	4c 39 05 00 00 00 00 	cmp    %r8,0x0(%rip)        # 110 <_dl_allocate_static_tls+0x20>	10c: R_X86_64_PC32	_dl_tls_static_align-0x4
     110:	0f 82 7f 00 00 00    	jb     195 <_dl_allocate_static_tls+0xa5>
     116:	48 8b 15 00 00 00 00 	mov    0x0(%rip),%rdx        # 11d <_dl_allocate_static_tls+0x2d>	119: R_X86_64_PC32	_dl_tls_static_used-0x4
     11d:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 124 <_dl_allocate_static_tls+0x34>	120: R_X86_64_PC32	_dl_tls_static_size-0x4
     124:	48 29 d0             	sub    %rdx,%rax
     127:	48 3d 3f 09 00 00    	cmp    $0x93f,%rax
     12d:	76 66                	jbe    195 <_dl_allocate_static_tls+0xa5>
     12f:	48 8b 8f 80 04 00 00 	mov    0x480(%rdi),%rcx
     136:	4c 8b 8f 70 04 00 00 	mov    0x470(%rdi),%r9
     13d:	48 2d 40 09 00 00    	sub    $0x940,%rax
     143:	49 01 c9             	add    %rcx,%r9
     146:	4c 39 c8             	cmp    %r9,%rax
     149:	72 4a                	jb     195 <_dl_allocate_static_tls+0xa5>
     14b:	48 01 c2             	add    %rax,%rdx
     14e:	4c 29 c8             	sub    %r9,%rax
     151:	48 89 d6             	mov    %rdx,%rsi
     154:	31 d2                	xor    %edx,%edx
     156:	48 29 ce             	sub    %rcx,%rsi
     159:	48 89 c1             	mov    %rax,%rcx
     15c:	49 f7 f0             	div    %r8
     15f:	48 89 c8             	mov    %rcx,%rax
     162:	48 29 d0             	sub    %rdx,%rax
     165:	48 29 c6             	sub    %rax,%rsi
     168:	48 8b 47 28          	mov    0x28(%rdi),%rax
     16c:	48 89 35 00 00 00 00 	mov    %rsi,0x0(%rip)        # 173 <_dl_allocate_static_tls+0x83>	16f: R_X86_64_PC32	_dl_tls_static_used-0x4
     173:	48 89 b7 88 04 00 00 	mov    %rsi,0x488(%rdi)
     17a:	f6 80 54 03 00 00 08 	testb  $0x8,0x354(%rax)
     181:	75 0d                	jne    190 <_dl_allocate_static_tls+0xa0>
     183:	80 8f 55 03 00 00 80 	orb    $0x80,0x355(%rdi)
     18a:	c3                   	ret
     18b:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
     190:	e9 00 00 00 00       	jmp    195 <_dl_allocate_static_tls+0xa5>	191: R_X86_64_PLT32	_dl_init_static_tls-0x4
     195:	55                   	push   %rbp
     196:	48 8b 77 08          	mov    0x8(%rdi),%rsi
     19a:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 1a1 <_dl_allocate_static_tls+0xb1>	19d: R_X86_64_PC32	.LC0-0x4
     1a1:	31 d2                	xor    %edx,%edx
     1a3:	31 ff                	xor    %edi,%edi
     1a5:	48 89 e5             	mov    %rsp,%rbp
     1a8:	e8 00 00 00 00       	call   1ad <_dl_allocate_static_tls+0xbd>	1a9: R_X86_64_PLT32	_dl_signal_error-0x4
     1ad:	0f 1f 00             	nopl   (%rax)

00000000000001b0 <_dl_protect_relro>:
     1b0:	f3 0f 1e fa          	endbr64
     1b4:	55                   	push   %rbp
     1b5:	48 89 e5             	mov    %rsp,%rbp
     1b8:	53                   	push   %rbx
     1b9:	48 89 fb             	mov    %rdi,%rbx
     1bc:	48 83 ec 08          	sub    $0x8,%rsp
     1c0:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 1c7 <_dl_protect_relro+0x17>	1c3: R_X86_64_PC32	_dl_pagesize-0x4
     1c7:	48 8b b7 a0 04 00 00 	mov    0x4a0(%rdi),%rsi
     1ce:	48 03 37             	add    (%rdi),%rsi
     1d1:	48 f7 d8             	neg    %rax
     1d4:	48 89 f7             	mov    %rsi,%rdi
     1d7:	48 03 b3 a8 04 00 00 	add    0x4a8(%rbx),%rsi
     1de:	48 21 c7             	and    %rax,%rdi
     1e1:	48 21 c6             	and    %rax,%rsi
     1e4:	48 39 f7             	cmp    %rsi,%rdi
     1e7:	75 07                	jne    1f0 <_dl_protect_relro+0x40>
     1e9:	48 8b 5d f8          	mov    -0x8(%rbp),%rbx
     1ed:	c9                   	leave
     1ee:	c3                   	ret
     1ef:	90                   	nop
     1f0:	48 29 fe             	sub    %rdi,%rsi
     1f3:	ba 01 00 00 00       	mov    $0x1,%edx
     1f8:	e8 00 00 00 00       	call   1fd <_dl_protect_relro+0x4d>	1f9: R_X86_64_PLT32	__mprotect-0x4
     1fd:	85 c0                	test   %eax,%eax
     1ff:	79 e8                	jns    1e9 <_dl_protect_relro+0x39>
     201:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 208 <_dl_protect_relro+0x58>	204: R_X86_64_GOTTPOFF	__libc_errno-0x4
     208:	48 8b 73 08          	mov    0x8(%rbx),%rsi
     20c:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 213 <_dl_protect_relro+0x63>	20f: R_X86_64_PC32	errstring.1-0x4
     213:	31 d2                	xor    %edx,%edx
     215:	64 8b 38             	mov    %fs:(%rax),%edi
     218:	e8 00 00 00 00       	call   21d <_dl_protect_relro+0x6d>	219: R_X86_64_PLT32	_dl_signal_error-0x4
     21d:	0f 1f 00             	nopl   (%rax)

0000000000000220 <_dl_reloc_bad_type>:
     220:	f3 0f 1e fa          	endbr64
     224:	55                   	push   %rbp
     225:	48 63 d2             	movslq %edx,%rdx
     228:	48 8d 04 d2          	lea    (%rdx,%rdx,8),%rax
     22c:	48 8d 14 42          	lea    (%rdx,%rax,2),%rdx
     230:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 237 <_dl_reloc_bad_type+0x17>	233: R_X86_64_PC32	.rodata+0x25c
     237:	48 89 e5             	mov    %rsp,%rbp
     23a:	41 55                	push   %r13
     23c:	41 54                	push   %r12
     23e:	4c 8d 6d b0          	lea    -0x50(%rbp),%r13
     242:	49 89 fc             	mov    %rdi,%r12
     245:	53                   	push   %rbx
     246:	4c 89 ef             	mov    %r13,%rdi
     249:	89 f3                	mov    %esi,%ebx
     24b:	48 8d 34 50          	lea    (%rax,%rdx,2),%rsi
     24f:	48 83 ec 38          	sub    $0x38,%rsp
     253:	e8 00 00 00 00       	call   258 <_dl_reloc_bad_type+0x38>	254: R_X86_64_PLT32	__stpcpy-0x4
     258:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 25f <_dl_reloc_bad_type+0x3f>	25b: R_X86_64_PC32	_itoa_lower_digits-0x4
     25f:	81 fb ff 00 00 00    	cmp    $0xff,%ebx
     265:	77 2b                	ja     292 <_dl_reloc_bad_type+0x72>
     267:	89 d9                	mov    %ebx,%ecx
     269:	83 e3 0f             	and    $0xf,%ebx
     26c:	31 d2                	xor    %edx,%edx
     26e:	c6 40 02 00          	movb   $0x0,0x2(%rax)
     272:	c1 e9 04             	shr    $0x4,%ecx
     275:	31 ff                	xor    %edi,%edi
     277:	83 e1 0f             	and    $0xf,%ecx
     27a:	8a 14 0e             	mov    (%rsi,%rcx,1),%dl
     27d:	8a 34 1e             	mov    (%rsi,%rbx,1),%dh
     280:	4c 89 e9             	mov    %r13,%rcx
     283:	49 8b 74 24 08       	mov    0x8(%r12),%rsi
     288:	66 89 10             	mov    %dx,(%rax)
     28b:	31 d2                	xor    %edx,%edx
     28d:	e8 00 00 00 00       	call   292 <_dl_reloc_bad_type+0x72>	28e: R_X86_64_PLT32	_dl_signal_error-0x4
     292:	89 da                	mov    %ebx,%edx
     294:	41 89 d8             	mov    %ebx,%r8d
     297:	89 df                	mov    %ebx,%edi
     299:	89 d9                	mov    %ebx,%ecx
     29b:	c1 ea 10             	shr    $0x10,%edx
     29e:	41 c1 e8 14          	shr    $0x14,%r8d
     2a2:	48 83 c0 06          	add    $0x6,%rax
     2a6:	c1 ef 18             	shr    $0x18,%edi
     2a9:	83 e2 0f             	and    $0xf,%edx
     2ac:	41 83 e0 0f          	and    $0xf,%r8d
     2b0:	c1 e9 1c             	shr    $0x1c,%ecx
     2b3:	0f b6 14 16          	movzbl (%rsi,%rdx,1),%edx
     2b7:	46 0f b6 04 06       	movzbl (%rsi,%r8,1),%r8d
     2bc:	83 e7 0f             	and    $0xf,%edi
     2bf:	0f b6 3c 3e          	movzbl (%rsi,%rdi,1),%edi
     2c3:	0f b6 0c 0e          	movzbl (%rsi,%rcx,1),%ecx
     2c7:	c1 e2 08             	shl    $0x8,%edx
     2ca:	44 09 c2             	or     %r8d,%edx
     2cd:	c1 e2 08             	shl    $0x8,%edx
     2d0:	09 fa                	or     %edi,%edx
     2d2:	c1 e2 08             	shl    $0x8,%edx
     2d5:	09 ca                	or     %ecx,%edx
     2d7:	89 d9                	mov    %ebx,%ecx
     2d9:	c1 e9 0c             	shr    $0xc,%ecx
     2dc:	89 50 fa             	mov    %edx,-0x6(%rax)
     2df:	31 d2                	xor    %edx,%edx
     2e1:	83 e1 0f             	and    $0xf,%ecx
     2e4:	8a 14 0e             	mov    (%rsi,%rcx,1),%dl
     2e7:	89 d9                	mov    %ebx,%ecx
     2e9:	c1 e9 08             	shr    $0x8,%ecx
     2ec:	83 e1 0f             	and    $0xf,%ecx
     2ef:	8a 34 0e             	mov    (%rsi,%rcx,1),%dh
     2f2:	66 89 50 fe          	mov    %dx,-0x2(%rax)
     2f6:	e9 6c ff ff ff       	jmp    267 <_dl_reloc_bad_type+0x47>
     2fb:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)

0000000000000300 <_dl_relocate_object>:
     300:	f3 0f 1e fa          	endbr64
     304:	55                   	push   %rbp
     305:	48 89 e5             	mov    %rsp,%rbp
     308:	41 57                	push   %r15
     30a:	41 56                	push   %r14
     30c:	41 55                	push   %r13
     30e:	41 54                	push   %r12
     310:	53                   	push   %rbx
     311:	48 81 ec d8 00 00 00 	sub    $0xd8,%rsp
     318:	48 89 b5 58 ff ff ff 	mov    %rsi,-0xa8(%rbp)
     31f:	89 8d 34 ff ff ff    	mov    %ecx,-0xcc(%rbp)
     325:	f6 87 54 03 00 00 08 	testb  $0x8,0x354(%rdi)
     32c:	0f 85 3b 04 00 00    	jne    76d <_dl_relocate_object+0x46d>
     332:	8b 05 00 00 00 00    	mov    0x0(%rip),%eax        # 338 <_dl_relocate_object+0x38>	334: R_X86_64_PC32	_dl_debug_mask-0x4
     338:	41 89 d5             	mov    %edx,%r13d
     33b:	49 89 fa             	mov    %rdi,%r10
     33e:	41 89 d4             	mov    %edx,%r12d
     341:	41 83 e5 01          	and    $0x1,%r13d
     345:	83 e0 20             	and    $0x20,%eax
     348:	85 c9                	test   %ecx,%ecx
     34a:	0f 84 30 04 00 00    	je     780 <_dl_relocate_object+0x480>
     350:	85 c0                	test   %eax,%eax
     352:	0f 85 73 18 00 00    	jne    1bcb <_dl_relocate_object+0x18cb>
     358:	49 8b 9a f0 00 00 00 	mov    0xf0(%r10),%rbx
     35f:	48 85 db             	test   %rbx,%rbx
     362:	0f 85 2b 17 00 00    	jne    1a93 <_dl_relocate_object+0x1793>
     368:	49 8b b2 f8 00 00 00 	mov    0xf8(%r10),%rsi
     36f:	48 85 f6             	test   %rsi,%rsi
     372:	74 6c                	je     3e0 <_dl_relocate_object+0xe0>
     374:	45 85 ed             	test   %r13d,%r13d
     377:	74 67                	je     3e0 <_dl_relocate_object+0xe0>
     379:	49 8b 42 58          	mov    0x58(%r10),%rax
     37d:	48 8b 40 08          	mov    0x8(%rax),%rax
     381:	41 f6 82 56 03 00 00 20 	testb  $0x20,0x356(%r10)
     389:	74 03                	je     38e <_dl_relocate_object+0x8e>
     38b:	49 03 02             	add    (%r10),%rax
     38e:	48 8b 50 08          	mov    0x8(%rax),%rdx
     392:	48 85 d2             	test   %rdx,%rdx
     395:	74 15                	je     3ac <_dl_relocate_object+0xac>
     397:	49 03 12             	add    (%r10),%rdx
     39a:	49 89 92 28 04 00 00 	mov    %rdx,0x428(%r10)
     3a1:	48 8d 50 18          	lea    0x18(%rax),%rdx
     3a5:	49 89 92 30 04 00 00 	mov    %rdx,0x430(%r10)
     3ac:	4c 89 50 08          	mov    %r10,0x8(%rax)
     3b0:	48 83 3d 00 00 00 00 00 	cmpq   $0x0,0x0(%rip)        # 3b8 <_dl_relocate_object+0xb8>	3b3: R_X86_64_PC32	_dl_x86_cpu_features+0x15b
     3b8:	0f 84 d2 0e 00 00    	je     1290 <_dl_relocate_object+0xf90>
     3be:	f6 05 00 00 00 00 02 	testb  $0x2,0x0(%rip)        # 3c5 <_dl_relocate_object+0xc5>	3c0: R_X86_64_PC32	_dl_x86_cpu_features+0x7f
     3c5:	48 8d 15 00 00 00 00 	lea    0x0(%rip),%rdx        # 3cc <_dl_relocate_object+0xcc>	3c8: R_X86_64_PC32	_dl_runtime_resolve_xsave-0x4
     3cc:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 3d3 <_dl_relocate_object+0xd3>	3cf: R_X86_64_PC32	_dl_runtime_resolve_xsavec-0x4
     3d3:	48 0f 45 d1          	cmovne %rcx,%rdx
     3d7:	48 89 50 10          	mov    %rdx,0x10(%rax)
     3db:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
     3e0:	41 0f b6 8a 56 03 00 00 	movzbl 0x356(%r10),%ecx
     3e8:	4d 8b 32             	mov    (%r10),%r14
     3eb:	41 89 c8             	mov    %ecx,%r8d
     3ee:	41 83 e0 20          	and    $0x20,%r8d
     3f2:	4c 3b 15 00 00 00 00 	cmp    0x0(%rip),%r10        # 3f9 <_dl_relocate_object+0xf9>	3f5: R_X86_64_REX_GOTPCRELX	_dl_rtld_map-0x4
     3f9:	0f 84 9b 00 00 00    	je     49a <_dl_relocate_object+0x19a>
     3ff:	49 8b 92 60 01 00 00 	mov    0x160(%r10),%rdx
     406:	48 85 d2             	test   %rdx,%rdx
     409:	0f 84 8b 00 00 00    	je     49a <_dl_relocate_object+0x19a>
     40f:	48 8b 52 08          	mov    0x8(%rdx),%rdx
     413:	45 84 c0             	test   %r8b,%r8b
     416:	4a 8d 3c 32          	lea    (%rdx,%r14,1),%rdi
     41a:	48 0f 44 fa          	cmove  %rdx,%rdi
     41e:	49 8b 92 58 01 00 00 	mov    0x158(%r10),%rdx
     425:	4c 8b 4a 08          	mov    0x8(%rdx),%r9
     429:	49 01 f9             	add    %rdi,%r9
     42c:	4c 39 cf             	cmp    %r9,%rdi
     42f:	73 69                	jae    49a <_dl_relocate_object+0x19a>
     431:	45 31 c0             	xor    %r8d,%r8d
     434:	eb 1d                	jmp    453 <_dl_relocate_object+0x153>
     436:	66 2e 0f 1f 84 00 00 00 00 00 	cs nopw 0x0(%rax,%rax,1)
     440:	4c 01 f0             	add    %r14,%rax
     443:	48 83 c7 08          	add    $0x8,%rdi
     447:	4c 01 30             	add    %r14,(%rax)
     44a:	4c 8d 40 08          	lea    0x8(%rax),%r8
     44e:	4c 39 cf             	cmp    %r9,%rdi
     451:	73 35                	jae    488 <_dl_relocate_object+0x188>
     453:	48 8b 07             	mov    (%rdi),%rax
     456:	a8 01                	test   $0x1,%al
     458:	74 e6                	je     440 <_dl_relocate_object+0x140>
     45a:	48 d1 e8             	shr    $1,%rax
     45d:	4c 89 c2             	mov    %r8,%rdx
     460:	74 16                	je     478 <_dl_relocate_object+0x178>
     462:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
     468:	a8 01                	test   $0x1,%al
     46a:	74 03                	je     46f <_dl_relocate_object+0x16f>
     46c:	4c 01 32             	add    %r14,(%rdx)
     46f:	48 83 c2 08          	add    $0x8,%rdx
     473:	48 d1 e8             	shr    $1,%rax
     476:	75 f0                	jne    468 <_dl_relocate_object+0x168>
     478:	48 83 c7 08          	add    $0x8,%rdi
     47c:	49 81 c0 f8 01 00 00 	add    $0x1f8,%r8
     483:	4c 39 cf             	cmp    %r9,%rdi
     486:	72 cb                	jb     453 <_dl_relocate_object+0x153>
     488:	41 0f b6 8a 56 03 00 00 	movzbl 0x356(%r10),%ecx
     490:	4d 8b 32             	mov    (%r10),%r14
     493:	41 89 c8             	mov    %ecx,%r8d
     496:	41 83 e0 20          	and    $0x20,%r8d
     49a:	49 8b 52 78          	mov    0x78(%r10),%rdx
     49e:	66 0f ef c0          	pxor   %xmm0,%xmm0
     4a2:	0f 29 45 90          	movaps %xmm0,-0x70(%rbp)
     4a6:	0f 29 45 a0          	movaps %xmm0,-0x60(%rbp)
     4aa:	0f 29 45 b0          	movaps %xmm0,-0x50(%rbp)
     4ae:	0f 29 45 c0          	movaps %xmm0,-0x40(%rbp)
     4b2:	48 85 d2             	test   %rdx,%rdx
     4b5:	0f 84 4d 0d 00 00    	je     1208 <_dl_relocate_object+0xf08>
     4bb:	48 8b 52 08          	mov    0x8(%rdx),%rdx
     4bf:	31 ff                	xor    %edi,%edi
     4c1:	48 85 d2             	test   %rdx,%rdx
     4c4:	74 3c                	je     502 <_dl_relocate_object+0x202>
     4c6:	4a 8d 3c 32          	lea    (%rdx,%r14,1),%rdi
     4ca:	45 84 c0             	test   %r8b,%r8b
     4cd:	4d 8b 8a c0 01 00 00 	mov    0x1c0(%r10),%r9
     4d4:	48 0f 45 d7          	cmovne %rdi,%rdx
     4d8:	49 8b ba 80 00 00 00 	mov    0x80(%r10),%rdi
     4df:	48 8b 7f 08          	mov    0x8(%rdi),%rdi
     4e3:	66 48 0f 6e c2       	movq   %rdx,%xmm0
     4e8:	66 48 0f 6e f7       	movq   %rdi,%xmm6
     4ed:	66 0f 6c c6          	punpcklqdq %xmm6,%xmm0
     4f1:	0f 29 45 90          	movaps %xmm0,-0x70(%rbp)
     4f5:	4d 85 c9             	test   %r9,%r9
     4f8:	74 08                	je     502 <_dl_relocate_object+0x202>
     4fa:	4d 8b 49 08          	mov    0x8(%r9),%r9
     4fe:	4c 89 4d a0          	mov    %r9,-0x60(%rbp)
     502:	49 83 ba e0 00 00 00 00 	cmpq   $0x0,0xe0(%r10)
     50a:	74 5e                	je     56a <_dl_relocate_object+0x26a>
     50c:	48 8b 76 08          	mov    0x8(%rsi),%rsi
     510:	45 84 c0             	test   %r8b,%r8b
     513:	4d 8b 42 50          	mov    0x50(%r10),%r8
     517:	4e 8d 0c 36          	lea    (%rsi,%r14,1),%r9
     51b:	4d 8b 40 08          	mov    0x8(%r8),%r8
     51f:	49 0f 45 f1          	cmovne %r9,%rsi
     523:	48 85 d2             	test   %rdx,%rdx
     526:	66 49 0f 6e e0       	movq   %r8,%xmm4
     52b:	48 0f 44 d6          	cmove  %rsi,%rdx
     52f:	66 48 0f 6e c6       	movq   %rsi,%xmm0
     534:	49 8d 04 30          	lea    (%r8,%rsi,1),%rax
     538:	66 0f 6c c4          	punpcklqdq %xmm4,%xmm0
     53c:	4c 8d 0c 3a          	lea    (%rdx,%rdi,1),%r9
     540:	48 89 55 90          	mov    %rdx,-0x70(%rbp)
     544:	49 39 c1             	cmp    %rax,%r9
     547:	75 0b                	jne    554 <_dl_relocate_object+0x254>
     549:	4c 29 c7             	sub    %r8,%rdi
     54c:	48 89 7d 98          	mov    %rdi,-0x68(%rbp)
     550:	4c 8d 0c 3a          	lea    (%rdx,%rdi,1),%r9
     554:	45 85 ed             	test   %r13d,%r13d
     557:	75 09                	jne    562 <_dl_relocate_object+0x262>
     559:	4c 39 ce             	cmp    %r9,%rsi
     55c:	0f 84 3e 0d 00 00    	je     12a0 <_dl_relocate_object+0xfa0>
     562:	44 89 6d c8          	mov    %r13d,-0x38(%rbp)
     566:	0f 29 45 b0          	movaps %xmm0,-0x50(%rbp)
     56a:	41 81 e4 00 00 00 02 	and    $0x2000000,%r12d
     571:	48 8d 45 d0          	lea    -0x30(%rbp),%rax
     575:	48 89 9d 20 ff ff ff 	mov    %rbx,-0xe0(%rbp)
     57c:	4d 89 f5             	mov    %r14,%r13
     57f:	44 89 a5 30 ff ff ff 	mov    %r12d,-0xd0(%rbp)
     586:	4c 8d 65 90          	lea    -0x70(%rbp),%r12
     58a:	48 89 85 28 ff ff ff 	mov    %rax,-0xd8(%rbp)
     591:	48 8d 45 88          	lea    -0x78(%rbp),%rax
     595:	48 89 85 48 ff ff ff 	mov    %rax,-0xb8(%rbp)
     59c:	4c 89 e0             	mov    %r12,%rax
     59f:	4d 89 d4             	mov    %r10,%r12
     5a2:	48 8b 70 10          	mov    0x10(%rax),%rsi
     5a6:	48 8b 10             	mov    (%rax),%rdx
     5a9:	48 8b 78 08          	mov    0x8(%rax),%rdi
     5ad:	48 8d 34 76          	lea    (%rsi,%rsi,2),%rsi
     5b1:	48 8d 1c f2          	lea    (%rdx,%rsi,8),%rbx
     5b5:	49 8b 74 24 70       	mov    0x70(%r12),%rsi
     5ba:	48 01 d7             	add    %rdx,%rdi
     5bd:	83 e1 20             	and    $0x20,%ecx
     5c0:	48 89 bd 70 ff ff ff 	mov    %rdi,-0x90(%rbp)
     5c7:	4c 8b 46 08          	mov    0x8(%rsi),%r8
     5cb:	4b 8d 34 28          	lea    (%r8,%r13,1),%rsi
     5cf:	49 0f 44 f0          	cmove  %r8,%rsi
     5d3:	44 8b 40 18          	mov    0x18(%rax),%r8d
     5d7:	48 89 b5 78 ff ff ff 	mov    %rsi,-0x88(%rbp)
     5de:	45 85 c0             	test   %r8d,%r8d
     5e1:	0f 84 49 03 00 00    	je     930 <_dl_relocate_object+0x630>
     5e7:	45 31 ff             	xor    %r15d,%r15d
     5ea:	45 31 db             	xor    %r11d,%r11d
     5ed:	48 39 fb             	cmp    %rdi,%rbx
     5f0:	0f 83 c3 00 00 00    	jae    6b9 <_dl_relocate_object+0x3b9>
     5f6:	4d 89 fa             	mov    %r15,%r10
     5f9:	4c 8b bd 70 ff ff ff 	mov    -0x90(%rbp),%r15
     600:	48 89 85 70 ff ff ff 	mov    %rax,-0x90(%rbp)
     607:	eb 34                	jmp    63d <_dl_relocate_object+0x33d>
     609:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
     610:	4c 8b 33             	mov    (%rbx),%r14
     613:	4d 01 ee             	add    %r13,%r14
     616:	48 83 f8 07          	cmp    $0x7,%rax
     61a:	0f 85 98 01 00 00    	jne    7b8 <_dl_relocate_object+0x4b8>
     620:	49 8b 84 24 28 04 00 00 	mov    0x428(%r12),%rax
     628:	48 85 c0             	test   %rax,%rax
     62b:	0f 85 e7 02 00 00    	jne    918 <_dl_relocate_object+0x618>
     631:	4d 01 2e             	add    %r13,(%r14)
     634:	48 83 c3 18          	add    $0x18,%rbx
     638:	4c 39 fb             	cmp    %r15,%rbx
     63b:	73 23                	jae    660 <_dl_relocate_object+0x360>
     63d:	48 8b 4b 08          	mov    0x8(%rbx),%rcx
     641:	89 c8                	mov    %ecx,%eax
     643:	83 f9 25             	cmp    $0x25,%ecx
     646:	75 c8                	jne    610 <_dl_relocate_object+0x310>
     648:	49 89 da             	mov    %rbx,%r10
     64b:	4d 85 db             	test   %r11,%r11
     64e:	75 e4                	jne    634 <_dl_relocate_object+0x334>
     650:	49 89 db             	mov    %rbx,%r11
     653:	48 83 c3 18          	add    $0x18,%rbx
     657:	4c 39 fb             	cmp    %r15,%rbx
     65a:	72 e1                	jb     63d <_dl_relocate_object+0x33d>
     65c:	0f 1f 40 00          	nopl   0x0(%rax)
     660:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
     667:	4d 89 d7             	mov    %r10,%r15
     66a:	4d 85 db             	test   %r11,%r11
     66d:	74 4a                	je     6b9 <_dl_relocate_object+0x3b9>
     66f:	4d 39 da             	cmp    %r11,%r10
     672:	72 45                	jb     6b9 <_dl_relocate_object+0x3b9>
     674:	44 8b 8d 30 ff ff ff 	mov    -0xd0(%rbp),%r9d
     67b:	45 85 c9             	test   %r9d,%r9d
     67e:	0f 85 a4 09 00 00    	jne    1028 <_dl_relocate_object+0xd28>
     684:	48 89 85 78 ff ff ff 	mov    %rax,-0x88(%rbp)
     68b:	4c 89 db             	mov    %r11,%rbx
     68e:	66 90                	xchg   %ax,%ax
     690:	83 7b 08 25          	cmpl   $0x25,0x8(%rbx)
     694:	75 13                	jne    6a9 <_dl_relocate_object+0x3a9>
     696:	4c 8b 33             	mov    (%rbx),%r14
     699:	48 8b 43 10          	mov    0x10(%rbx),%rax
     69d:	49 03 04 24          	add    (%r12),%rax
     6a1:	ff d0                	call   *%rax
     6a3:	4d 01 ee             	add    %r13,%r14
     6a6:	49 89 06             	mov    %rax,(%r14)
     6a9:	48 83 c3 18          	add    $0x18,%rbx
     6ad:	49 39 df             	cmp    %rbx,%r15
     6b0:	73 de                	jae    690 <_dl_relocate_object+0x390>
     6b2:	48 8b 85 78 ff ff ff 	mov    -0x88(%rbp),%rax
     6b9:	48 83 c0 20          	add    $0x20,%rax
     6bd:	48 39 85 28 ff ff ff 	cmp    %rax,-0xd8(%rbp)
     6c4:	74 1a                	je     6e0 <_dl_relocate_object+0x3e0>
     6c6:	41 0f b6 8c 24 56 03 00 00 	movzbl 0x356(%r12),%ecx
     6cf:	4d 8b 2c 24          	mov    (%r12),%r13
     6d3:	e9 ca fe ff ff       	jmp    5a2 <_dl_relocate_object+0x2a2>
     6d8:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
     6e0:	8b 85 34 ff ff ff    	mov    -0xcc(%rbp),%eax
     6e6:	48 8b 9d 20 ff ff ff 	mov    -0xe0(%rbp),%rbx
     6ed:	4d 89 e2             	mov    %r12,%r10
     6f0:	85 c0                	test   %eax,%eax
     6f2:	74 58                	je     74c <_dl_relocate_object+0x44c>
     6f4:	49 8b 44 24 50       	mov    0x50(%r12),%rax
     6f9:	48 85 c0             	test   %rax,%rax
     6fc:	74 4e                	je     74c <_dl_relocate_object+0x44c>
     6fe:	49 8b 94 24 e0 00 00 00 	mov    0xe0(%r12),%rdx
     706:	31 c9                	xor    %ecx,%ecx
     708:	48 8b 40 08          	mov    0x8(%rax),%rax
     70c:	bf 20 00 00 00       	mov    $0x20,%edi
     711:	4c 89 a5 78 ff ff ff 	mov    %r12,-0x88(%rbp)
     718:	48 83 7a 08 07       	cmpq   $0x7,0x8(%rdx)
     71d:	0f 94 c1             	sete   %cl
     720:	31 d2                	xor    %edx,%edx
     722:	48 8d 0c cd 10 00 00 00 	lea    0x10(,%rcx,8),%rcx
     72a:	48 f7 f1             	div    %rcx
     72d:	48 89 c6             	mov    %rax,%rsi
     730:	e8 00 00 00 00       	call   735 <_dl_relocate_object+0x435>	731: R_X86_64_PLT32	calloc-0x4
     735:	4c 8b 95 78 ff ff ff 	mov    -0x88(%rbp),%r10
     73c:	49 89 82 78 03 00 00 	mov    %rax,0x378(%r10)
     743:	48 85 c0             	test   %rax,%rax
     746:	0f 84 2a 26 00 00    	je     2d76 <_dl_relocate_object+0x2a76>
     74c:	41 80 8a 54 03 00 00 08 	orb    $0x8,0x354(%r10)
     754:	48 85 db             	test   %rbx,%rbx
     757:	0f 85 ca 14 00 00    	jne    1c27 <_dl_relocate_object+0x1927>
     75d:	49 8b 82 a8 04 00 00 	mov    0x4a8(%r10),%rax
     764:	48 85 c0             	test   %rax,%rax
     767:	0f 85 ab 0a 00 00    	jne    1218 <_dl_relocate_object+0xf18>
     76d:	48 8d 65 d8          	lea    -0x28(%rbp),%rsp
     771:	5b                   	pop    %rbx
     772:	41 5c                	pop    %r12
     774:	41 5d                	pop    %r13
     776:	41 5e                	pop    %r14
     778:	41 5f                	pop    %r15
     77a:	5d                   	pop    %rbp
     77b:	c3                   	ret
     77c:	0f 1f 40 00          	nopl   0x0(%rax)
     780:	48 83 bf 00 01 00 00 00 	cmpq   $0x0,0x100(%rdi)
     788:	0f 84 c2 fb ff ff    	je     350 <_dl_relocate_object+0x50>
     78e:	85 c0                	test   %eax,%eax
     790:	0f 85 9d 24 00 00    	jne    2c33 <_dl_relocate_object+0x2933>
     796:	48 8b 9f f0 00 00 00 	mov    0xf0(%rdi),%rbx
     79d:	48 85 db             	test   %rbx,%rbx
     7a0:	0f 85 ea 12 00 00    	jne    1a90 <_dl_relocate_object+0x1790>
     7a6:	49 8b b2 f8 00 00 00 	mov    0xf8(%r10),%rsi
     7ad:	45 31 ed             	xor    %r13d,%r13d
     7b0:	e9 2b fc ff ff       	jmp    3e0 <_dl_relocate_object+0xe0>
     7b5:	0f 1f 00             	nopl   (%rax)
     7b8:	48 83 f8 24          	cmp    $0x24,%rax
     7bc:	0f 85 0f 12 00 00    	jne    19d1 <_dl_relocate_object+0x16d1>
     7c2:	49 8b 44 24 70       	mov    0x70(%r12),%rax
     7c7:	48 c1 e9 20          	shr    $0x20,%rcx
     7cb:	31 d2                	xor    %edx,%edx
     7cd:	48 8b 70 08          	mov    0x8(%rax),%rsi
     7d1:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
     7da:	74 07                	je     7e3 <_dl_relocate_object+0x4e3>
     7dc:	49 8b 14 24          	mov    (%r12),%rdx
     7e0:	48 01 d6             	add    %rdx,%rsi
     7e3:	48 8d 04 09          	lea    (%rcx,%rcx,1),%rax
     7e7:	4d 8b 84 24 08 02 00 00 	mov    0x208(%r12),%r8
     7ef:	48 01 c1             	add    %rax,%rcx
     7f2:	48 8d 3c ce          	lea    (%rsi,%rcx,8),%rdi
     7f6:	48 89 bd 78 ff ff ff 	mov    %rdi,-0x88(%rbp)
     7fd:	4d 85 c0             	test   %r8,%r8
     800:	74 1f                	je     821 <_dl_relocate_object+0x521>
     802:	48 01 d0             	add    %rdx,%rax
     805:	49 03 40 08          	add    0x8(%r8),%rax
     809:	0f b7 00             	movzwl (%rax),%eax
     80c:	25 ff 7f 00 00       	and    $0x7fff,%eax
     811:	48 8d 0c 40          	lea    (%rax,%rax,2),%rcx
     815:	49 8b 84 24 20 03 00 00 	mov    0x320(%r12),%rax
     81d:	4c 8d 04 c8          	lea    (%rax,%rcx,8),%r8
     821:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
     828:	48 89 75 88          	mov    %rsi,-0x78(%rbp)
     82c:	0f b6 46 04          	movzbl 0x4(%rsi),%eax
     830:	89 c7                	mov    %eax,%edi
     832:	40 c0 ef 04          	shr    $0x4,%dil
     836:	0f 84 64 08 00 00    	je     10a0 <_dl_relocate_object+0xda0>
     83c:	0f b6 4e 05          	movzbl 0x5(%rsi),%ecx
     840:	83 e1 03             	and    $0x3,%ecx
     843:	83 e9 01             	sub    $0x1,%ecx
     846:	83 f9 01             	cmp    $0x1,%ecx
     849:	0f 86 51 08 00 00    	jbe    10a0 <_dl_relocate_object+0xda0>
     84f:	49 3b b4 24 40 04 00 00 	cmp    0x440(%r12),%rsi
     857:	0f 84 8f 14 00 00    	je     1cec <_dl_relocate_object+0x19ec>
     85d:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
     864:	49 8b 44 24 68       	mov    0x68(%r12),%rax
     869:	41 c7 84 24 48 04 00 00 01 00 00 00 	movl   $0x1,0x448(%r12)
     875:	48 03 50 08          	add    0x8(%rax),%rdx
     879:	8b 06                	mov    (%rsi),%eax
     87b:	49 89 b4 24 40 04 00 00 	mov    %rsi,0x440(%r12)
     883:	48 8d 3c 02          	lea    (%rdx,%rax,1),%rdi
     887:	4d 85 c0             	test   %r8,%r8
     88a:	74 0c                	je     898 <_dl_relocate_object+0x598>
     88c:	41 8b 70 08          	mov    0x8(%r8),%esi
     890:	85 f6                	test   %esi,%esi
     892:	0f 84 f0 09 00 00    	je     1288 <_dl_relocate_object+0xf88>
     898:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
     89f:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
     8a6:	48 8d 55 88          	lea    -0x78(%rbp),%rdx
     8aa:	4c 89 e6             	mov    %r12,%rsi
     8ad:	4c 89 9d 68 ff ff ff 	mov    %r11,-0x98(%rbp)
     8b4:	41 b9 01 00 00 00    	mov    $0x1,%r9d
     8ba:	6a 00                	push   $0x0
     8bc:	6a 09                	push   $0x9
     8be:	e8 00 00 00 00       	call   8c3 <_dl_relocate_object+0x5c3>	8bf: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
     8c3:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
     8c7:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
     8ce:	66 48 0f 6e c0       	movq   %rax,%xmm0
     8d3:	4c 8b 9d 68 ff ff ff 	mov    -0x98(%rbp),%r11
     8da:	49 89 c1             	mov    %rax,%r9
     8dd:	66 48 0f 6e ea       	movq   %rdx,%xmm5
     8e2:	66 0f 6c c5          	punpcklqdq %xmm5,%xmm0
     8e6:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
     8ef:	58                   	pop    %rax
     8f0:	59                   	pop    %rcx
     8f1:	48 85 d2             	test   %rdx,%rdx
     8f4:	0f 85 e6 08 00 00    	jne    11e0 <_dl_relocate_object+0xee0>
     8fa:	48 8b 43 10          	mov    0x10(%rbx),%rax
     8fe:	49 89 46 08          	mov    %rax,0x8(%r14)
     902:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 909 <_dl_relocate_object+0x609>	905: R_X86_64_PC32	_dl_tlsdesc_undefweak-0x4
     909:	49 89 06             	mov    %rax,(%r14)
     90c:	e9 23 fd ff ff       	jmp    634 <_dl_relocate_object+0x334>
     911:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
     918:	4c 89 f2             	mov    %r14,%rdx
     91b:	49 2b 94 24 30 04 00 00 	sub    0x430(%r12),%rdx
     923:	48 8d 04 50          	lea    (%rax,%rdx,2),%rax
     927:	49 89 06             	mov    %rax,(%r14)
     92a:	e9 05 fd ff ff       	jmp    634 <_dl_relocate_object+0x334>
     92f:	90                   	nop
     930:	4c 3b 25 00 00 00 00 	cmp    0x0(%rip),%r12        # 937 <_dl_relocate_object+0x637>	933: R_X86_64_REX_GOTPCRELX	_dl_rtld_map-0x4
     937:	74 33                	je     96c <_dl_relocate_object+0x66c>
     939:	48 39 da             	cmp    %rbx,%rdx
     93c:	73 2e                	jae    96c <_dl_relocate_object+0x66c>
     93e:	66 90                	xchg   %ax,%ax
     940:	48 8b 32             	mov    (%rdx),%rsi
     943:	8b 4a 08             	mov    0x8(%rdx),%ecx
     946:	4c 01 ee             	add    %r13,%rsi
     949:	48 83 f9 26          	cmp    $0x26,%rcx
     94d:	74 0a                	je     959 <_dl_relocate_object+0x659>
     94f:	48 83 f9 08          	cmp    $0x8,%rcx
     953:	0f 85 e9 22 00 00    	jne    2c42 <_dl_relocate_object+0x2942>
     959:	48 8b 4a 10          	mov    0x10(%rdx),%rcx
     95d:	48 83 c2 18          	add    $0x18,%rdx
     961:	4c 01 e9             	add    %r13,%rcx
     964:	48 89 0e             	mov    %rcx,(%rsi)
     967:	48 39 da             	cmp    %rbx,%rdx
     96a:	72 d4                	jb     940 <_dl_relocate_object+0x640>
     96c:	4d 8b 94 24 08 02 00 00 	mov    0x208(%r12),%r10
     974:	4d 85 d2             	test   %r10,%r10
     977:	0f 84 33 09 00 00    	je     12b0 <_dl_relocate_object+0xfb0>
     97d:	49 8b 72 08          	mov    0x8(%r10),%rsi
     981:	48 89 b5 68 ff ff ff 	mov    %rsi,-0x98(%rbp)
     988:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
     991:	0f 85 59 08 00 00    	jne    11f0 <_dl_relocate_object+0xef0>
     997:	48 8b b5 70 ff ff ff 	mov    -0x90(%rbp),%rsi
     99e:	48 39 f3             	cmp    %rsi,%rbx
     9a1:	0f 83 12 fd ff ff    	jae    6b9 <_dl_relocate_object+0x3b9>
     9a7:	44 8b 85 30 ff ff ff 	mov    -0xd0(%rbp),%r8d
     9ae:	48 c7 85 38 ff ff ff 00 00 00 00 	movq   $0x0,-0xc8(%rbp)
     9b9:	48 c7 85 40 ff ff ff 00 00 00 00 	movq   $0x0,-0xc0(%rbp)
     9c4:	45 85 c0             	test   %r8d,%r8d
     9c7:	0f 85 5d 14 00 00    	jne    1e2a <_dl_relocate_object+0x1b2a>
     9cd:	4c 89 ad 60 ff ff ff 	mov    %r13,-0xa0(%rbp)
     9d4:	48 89 85 18 ff ff ff 	mov    %rax,-0xe8(%rbp)
     9db:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
     9e0:	4c 8b 7b 08          	mov    0x8(%rbx),%r15
     9e4:	48 8b bd 68 ff ff ff 	mov    -0x98(%rbp),%rdi
     9eb:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
     9f2:	49 8b b4 24 20 03 00 00 	mov    0x320(%r12),%rsi
     9fa:	4c 89 f8             	mov    %r15,%rax
     9fd:	4c 03 13             	add    (%rbx),%r10
     a00:	45 89 fd             	mov    %r15d,%r13d
     a03:	48 c1 e8 20          	shr    $0x20,%rax
     a07:	0f b7 14 47          	movzwl (%rdi,%rax,2),%edx
     a0b:	48 8b bd 78 ff ff ff 	mov    -0x88(%rbp),%rdi
     a12:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
     a16:	4c 8d 34 c7          	lea    (%rdi,%rax,8),%r14
     a1a:	41 83 ff 25          	cmp    $0x25,%r15d
     a1e:	0f 84 7c 05 00 00    	je     fa0 <_dl_relocate_object+0xca0>
     a24:	4c 89 75 88          	mov    %r14,-0x78(%rbp)
     a28:	49 83 fd 08          	cmp    $0x8,%r13
     a2c:	0f 84 96 05 00 00    	je     fc8 <_dl_relocate_object+0xcc8>
     a32:	49 83 fd 26          	cmp    $0x26,%r13
     a36:	0f 84 8c 05 00 00    	je     fc8 <_dl_relocate_object+0xcc8>
     a3c:	4d 85 ed             	test   %r13,%r13
     a3f:	0f 84 6b 01 00 00    	je     bb0 <_dl_relocate_object+0x8b0>
     a45:	41 0f b6 46 04       	movzbl 0x4(%r14),%eax
     a4a:	89 c7                	mov    %eax,%edi
     a4c:	40 c0 ef 04          	shr    $0x4,%dil
     a50:	0f 84 2a 05 00 00    	je     f80 <_dl_relocate_object+0xc80>
     a56:	41 0f b6 4e 05       	movzbl 0x5(%r14),%ecx
     a5b:	83 e1 03             	and    $0x3,%ecx
     a5e:	83 e9 01             	sub    $0x1,%ecx
     a61:	83 f9 01             	cmp    $0x1,%ecx
     a64:	0f 86 16 05 00 00    	jbe    f80 <_dl_relocate_object+0xc80>
     a6a:	4d 3b b4 24 40 04 00 00 	cmp    0x440(%r12),%r14
     a72:	0f 84 c8 05 00 00    	je     1040 <_dl_relocate_object+0xd40>
     a78:	49 83 fd 12          	cmp    $0x12,%r13
     a7c:	0f 87 5e 05 00 00    	ja     fe0 <_dl_relocate_object+0xce0>
     a82:	41 b9 01 00 00 00    	mov    $0x1,%r9d
     a88:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
     a8f:	75 17                	jne    aa8 <_dl_relocate_object+0x7a8>
     a91:	41 b9 02 00 00 00    	mov    $0x2,%r9d
     a97:	49 83 fd 05          	cmp    $0x5,%r13
     a9b:	74 0b                	je     aa8 <_dl_relocate_object+0x7a8>
     a9d:	45 31 c9             	xor    %r9d,%r9d
     aa0:	49 83 fd 07          	cmp    $0x7,%r13
     aa4:	41 0f 94 c1          	sete   %r9b
     aa8:	49 8b 44 24 68       	mov    0x68(%r12),%rax
     aad:	41 8b 3e             	mov    (%r14),%edi
     ab0:	45 89 8c 24 48 04 00 00 	mov    %r9d,0x448(%r12)
     ab8:	4d 89 b4 24 40 04 00 00 	mov    %r14,0x440(%r12)
     ac0:	48 8b 48 08          	mov    0x8(%rax),%rcx
     ac4:	31 c0                	xor    %eax,%eax
     ac6:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
     acf:	74 04                	je     ad5 <_dl_relocate_object+0x7d5>
     ad1:	49 8b 04 24          	mov    (%r12),%rax
     ad5:	81 e2 ff 7f 00 00    	and    $0x7fff,%edx
     adb:	48 01 cf             	add    %rcx,%rdi
     ade:	48 8d 14 52          	lea    (%rdx,%rdx,2),%rdx
     ae2:	48 01 c7             	add    %rax,%rdi
     ae5:	4c 8d 04 d6          	lea    (%rsi,%rdx,8),%r8
     ae9:	4d 85 c0             	test   %r8,%r8
     aec:	74 0c                	je     afa <_dl_relocate_object+0x7fa>
     aee:	45 8b 58 08          	mov    0x8(%r8),%r11d
     af2:	45 85 db             	test   %r11d,%r11d
     af5:	75 03                	jne    afa <_dl_relocate_object+0x7fa>
     af7:	45 31 c0             	xor    %r8d,%r8d
     afa:	4c 89 95 50 ff ff ff 	mov    %r10,-0xb0(%rbp)
     b01:	48 8b 95 48 ff ff ff 	mov    -0xb8(%rbp),%rdx
     b08:	4c 89 e6             	mov    %r12,%rsi
     b0b:	6a 00                	push   $0x0
     b0d:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
     b14:	6a 09                	push   $0x9
     b16:	e8 00 00 00 00       	call   b1b <_dl_relocate_object+0x81b>	b17: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
     b1b:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
     b1f:	66 48 0f 6e c0       	movq   %rax,%xmm0
     b24:	49 89 c3             	mov    %rax,%r11
     b27:	66 48 0f 6e ca       	movq   %rdx,%xmm1
     b2c:	66 0f 6c c1          	punpcklqdq %xmm1,%xmm0
     b30:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
     b39:	41 59                	pop    %r9
     b3b:	41 5a                	pop    %r10
     b3d:	4c 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%r10
     b44:	45 31 c9             	xor    %r9d,%r9d
     b47:	48 85 d2             	test   %rdx,%rdx
     b4a:	74 29                	je     b75 <_dl_relocate_object+0x875>
     b4c:	0f b7 4a 06          	movzwl 0x6(%rdx),%ecx
     b50:	0f b6 42 04          	movzbl 0x4(%rdx),%eax
     b54:	66 83 f9 f1          	cmp    $0xfff1,%cx
     b58:	0f 84 36 04 00 00    	je     f94 <_dl_relocate_object+0xc94>
     b5e:	4d 8b 0b             	mov    (%r11),%r9
     b61:	83 e0 0f             	and    $0xf,%eax
     b64:	4c 03 4a 08          	add    0x8(%rdx),%r9
     b68:	3c 0a                	cmp    $0xa,%al
     b6a:	75 09                	jne    b75 <_dl_relocate_object+0x875>
     b6c:	66 85 c9             	test   %cx,%cx
     b6f:	0f 85 ab 05 00 00    	jne    1120 <_dl_relocate_object+0xe20>
     b75:	49 83 fd 25          	cmp    $0x25,%r13
     b79:	77 15                	ja     b90 <_dl_relocate_object+0x890>
     b7b:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # b82 <_dl_relocate_object+0x882>	b7e: R_X86_64_PC32	.rodata-0x4
     b82:	4a 63 04 ae          	movslq (%rsi,%r13,4),%rax
     b86:	48 01 f0             	add    %rsi,%rax
     b89:	3e ff e0             	notrack jmp *%rax
     b8c:	0f 1f 40 00          	nopl   0x0(%rax)
     b90:	31 d2                	xor    %edx,%edx
     b92:	44 89 fe             	mov    %r15d,%esi
     b95:	4c 89 e7             	mov    %r12,%rdi
     b98:	e8 00 00 00 00       	call   b9d <_dl_relocate_object+0x89d>	b99: R_X86_64_PLT32	_dl_reloc_bad_type-0x4
     b9d:	0f 1f 00             	nopl   (%rax)
     ba0:	4c 03 4b 10          	add    0x10(%rbx),%r9
     ba4:	4d 89 0a             	mov    %r9,(%r10)
     ba7:	66 0f 1f 84 00 00 00 00 00 	nopw   0x0(%rax,%rax,1)
     bb0:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
     bb7:	48 83 c3 18          	add    $0x18,%rbx
     bbb:	48 39 c3             	cmp    %rax,%rbx
     bbe:	0f 82 1c fe ff ff    	jb     9e0 <_dl_relocate_object+0x6e0>
     bc4:	48 8b bd 40 ff ff ff 	mov    -0xc0(%rbp),%rdi
     bcb:	4c 8b ad 60 ff ff ff 	mov    -0xa0(%rbp),%r13
     bd2:	48 8b 85 18 ff ff ff 	mov    -0xe8(%rbp),%rax
     bd9:	48 85 ff             	test   %rdi,%rdi
     bdc:	0f 84 d7 fa ff ff    	je     6b9 <_dl_relocate_object+0x3b9>
     be2:	48 39 bd 38 ff ff ff 	cmp    %rdi,-0xc8(%rbp)
     be9:	0f 82 ca fa ff ff    	jb     6b9 <_dl_relocate_object+0x3b9>
     bef:	48 89 85 60 ff ff ff 	mov    %rax,-0xa0(%rbp)
     bf6:	4c 8b b5 38 ff ff ff 	mov    -0xc8(%rbp),%r14
     bfd:	48 89 fb             	mov    %rdi,%rbx
     c00:	eb 13                	jmp    c15 <_dl_relocate_object+0x915>
     c02:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
     c08:	48 83 c3 18          	add    $0x18,%rbx
     c0c:	49 39 de             	cmp    %rbx,%r14
     c0f:	0f 82 59 01 00 00    	jb     d6e <_dl_relocate_object+0xa6e>
     c15:	48 8b 43 08          	mov    0x8(%rbx),%rax
     c19:	83 f8 25             	cmp    $0x25,%eax
     c1c:	75 ea                	jne    c08 <_dl_relocate_object+0x908>
     c1e:	48 8b bd 68 ff ff ff 	mov    -0x98(%rbp),%rdi
     c25:	48 c1 e8 20          	shr    $0x20,%rax
     c29:	4c 8b 3b             	mov    (%rbx),%r15
     c2c:	49 8b 8c 24 20 03 00 00 	mov    0x320(%r12),%rcx
     c34:	0f b7 34 47          	movzwl (%rdi,%rax,2),%esi
     c38:	48 8b bd 78 ff ff ff 	mov    -0x88(%rbp),%rdi
     c3f:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
     c43:	4d 01 ef             	add    %r13,%r15
     c46:	4c 8d 14 c7          	lea    (%rdi,%rax,8),%r10
     c4a:	4c 89 55 88          	mov    %r10,-0x78(%rbp)
     c4e:	41 0f b6 42 04       	movzbl 0x4(%r10),%eax
     c53:	89 c7                	mov    %eax,%edi
     c55:	40 c0 ef 04          	shr    $0x4,%dil
     c59:	0f 84 91 03 00 00    	je     ff0 <_dl_relocate_object+0xcf0>
     c5f:	41 0f b6 52 05       	movzbl 0x5(%r10),%edx
     c64:	83 e2 03             	and    $0x3,%edx
     c67:	83 ea 01             	sub    $0x1,%edx
     c6a:	83 fa 01             	cmp    $0x1,%edx
     c6d:	0f 86 7d 03 00 00    	jbe    ff0 <_dl_relocate_object+0xcf0>
     c73:	4d 3b 94 24 40 04 00 00 	cmp    0x440(%r12),%r10
     c7b:	0f 84 27 0d 00 00    	je     19a8 <_dl_relocate_object+0x16a8>
     c81:	49 8b 44 24 68       	mov    0x68(%r12),%rax
     c86:	41 8b 3a             	mov    (%r10),%edi
     c89:	4d 89 94 24 40 04 00 00 	mov    %r10,0x440(%r12)
     c91:	31 d2                	xor    %edx,%edx
     c93:	41 c7 84 24 48 04 00 00 00 00 00 00 	movl   $0x0,0x448(%r12)
     c9f:	4c 8b 48 08          	mov    0x8(%rax),%r9
     ca3:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
     cac:	74 04                	je     cb2 <_dl_relocate_object+0x9b2>
     cae:	49 8b 14 24          	mov    (%r12),%rdx
     cb2:	48 89 f0             	mov    %rsi,%rax
     cb5:	4c 01 cf             	add    %r9,%rdi
     cb8:	25 ff 7f 00 00       	and    $0x7fff,%eax
     cbd:	48 01 d7             	add    %rdx,%rdi
     cc0:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
     cc4:	4c 8d 04 c1          	lea    (%rcx,%rax,8),%r8
     cc8:	4d 85 c0             	test   %r8,%r8
     ccb:	74 0c                	je     cd9 <_dl_relocate_object+0x9d9>
     ccd:	45 8b 58 08          	mov    0x8(%r8),%r11d
     cd1:	45 85 db             	test   %r11d,%r11d
     cd4:	75 03                	jne    cd9 <_dl_relocate_object+0x9d9>
     cd6:	45 31 c0             	xor    %r8d,%r8d
     cd9:	4c 89 95 70 ff ff ff 	mov    %r10,-0x90(%rbp)
     ce0:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
     ce7:	45 31 c9             	xor    %r9d,%r9d
     cea:	4c 89 e6             	mov    %r12,%rsi
     ced:	6a 00                	push   $0x0
     cef:	48 8b 95 48 ff ff ff 	mov    -0xb8(%rbp),%rdx
     cf6:	6a 09                	push   $0x9
     cf8:	e8 00 00 00 00       	call   cfd <_dl_relocate_object+0x9fd>	cf9: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
     cfd:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
     d01:	66 48 0f 6e c0       	movq   %rax,%xmm0
     d06:	48 89 c1             	mov    %rax,%rcx
     d09:	66 48 0f 6e da       	movq   %rdx,%xmm3
     d0e:	66 0f 6c c3          	punpcklqdq %xmm3,%xmm0
     d12:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
     d1b:	41 59                	pop    %r9
     d1d:	41 5a                	pop    %r10
     d1f:	4c 8b 95 70 ff ff ff 	mov    -0x90(%rbp),%r10
     d26:	48 85 d2             	test   %rdx,%rdx
     d29:	74 29                	je     d54 <_dl_relocate_object+0xa54>
     d2b:	0f b7 72 06          	movzwl 0x6(%rdx),%esi
     d2f:	0f b6 42 04          	movzbl 0x4(%rdx),%eax
     d33:	66 83 fe f1          	cmp    $0xfff1,%si
     d37:	0f 84 c7 02 00 00    	je     1004 <_dl_relocate_object+0xd04>
     d3d:	4c 8b 19             	mov    (%rcx),%r11
     d40:	83 e0 0f             	and    $0xf,%eax
     d43:	4c 8b 4a 08          	mov    0x8(%rdx),%r9
     d47:	3c 0a                	cmp    $0xa,%al
     d49:	75 09                	jne    d54 <_dl_relocate_object+0xa54>
     d4b:	66 85 f6             	test   %si,%si
     d4e:	0f 85 a0 0c 00 00    	jne    19f4 <_dl_relocate_object+0x16f4>
     d54:	48 8b 43 10          	mov    0x10(%rbx),%rax
     d58:	49 03 04 24          	add    (%r12),%rax
     d5c:	ff d0                	call   *%rax
     d5e:	48 83 c3 18          	add    $0x18,%rbx
     d62:	49 89 07             	mov    %rax,(%r15)
     d65:	49 39 de             	cmp    %rbx,%r14
     d68:	0f 83 a7 fe ff ff    	jae    c15 <_dl_relocate_object+0x915>
     d6e:	48 8b 85 60 ff ff ff 	mov    -0xa0(%rbp),%rax
     d75:	e9 3f f9 ff ff       	jmp    6b9 <_dl_relocate_object+0x3b9>
     d7a:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
     d80:	48 8b 45 88          	mov    -0x78(%rbp),%rax
     d84:	4c 8b 48 10          	mov    0x10(%rax),%r9
     d88:	4c 03 4b 10          	add    0x10(%rbx),%r9
     d8c:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
     d91:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # d98 <_dl_relocate_object+0xa98>	d94: R_X86_64_PC32	.LC8-0x4
     d98:	45 89 0a             	mov    %r9d,(%r10)
     d9b:	4c 39 c8             	cmp    %r9,%rax
     d9e:	0f 83 0c fe ff ff    	jae    bb0 <_dl_relocate_object+0x8b0>
     da4:	49 8b 44 24 68       	mov    0x68(%r12),%rax
     da9:	48 8b 40 08          	mov    0x8(%rax),%rax
     dad:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
     db6:	74 04                	je     dbc <_dl_relocate_object+0xabc>
     db8:	49 03 04 24          	add    (%r12),%rax
     dbc:	41 8b 16             	mov    (%r14),%edx
     dbf:	48 01 c2             	add    %rax,%rdx
     dc2:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # dc9 <_dl_relocate_object+0xac9>	dc5: R_X86_64_PC32	_dl_argv-0x4
     dc9:	48 8b 30             	mov    (%rax),%rsi
     dcc:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # dd3 <_dl_relocate_object+0xad3>	dcf: R_X86_64_PC32	.LC6-0x4
     dd3:	48 85 f6             	test   %rsi,%rsi
     dd6:	48 0f 44 f0          	cmove  %rax,%rsi
     dda:	31 c0                	xor    %eax,%eax
     ddc:	e8 00 00 00 00       	call   de1 <_dl_relocate_object+0xae1>	ddd: R_X86_64_PLT32	_dl_error_printf-0x4
     de1:	e9 ca fd ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     de6:	66 2e 0f 1f 84 00 00 00 00 00 	cs nopw 0x0(%rax,%rax,1)
     df0:	41 c6 84 24 59 03 00 00 01 	movb   $0x1,0x359(%r12)
     df9:	e9 a6 fd ff ff       	jmp    ba4 <_dl_relocate_object+0x8a4>
     dfe:	66 90                	xchg   %ax,%ax
     e00:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
     e04:	48 85 d2             	test   %rdx,%rdx
     e07:	0f 84 a3 fd ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     e0d:	48 8b 43 10          	mov    0x10(%rbx),%rax
     e11:	48 03 42 08          	add    0x8(%rdx),%rax
     e15:	49 89 02             	mov    %rax,(%r10)
     e18:	e9 93 fd ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     e1d:	0f 1f 00             	nopl   (%rax)
     e20:	4d 85 db             	test   %r11,%r11
     e23:	0f 84 87 fd ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     e29:	49 8b 83 90 04 00 00 	mov    0x490(%r11),%rax
     e30:	49 89 02             	mov    %rax,(%r10)
     e33:	e9 78 fd ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     e38:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
     e40:	48 8b 43 10          	mov    0x10(%rbx),%rax
     e44:	4c 29 d0             	sub    %r10,%rax
     e47:	4c 01 c8             	add    %r9,%rax
     e4a:	48 63 d0             	movslq %eax,%rdx
     e4d:	41 89 02             	mov    %eax,(%r10)
     e50:	48 39 d0             	cmp    %rdx,%rax
     e53:	0f 84 57 fd ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     e59:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # e60 <_dl_relocate_object+0xb60>	e5c: R_X86_64_PC32	.LC9-0x4
     e60:	e9 3f ff ff ff       	jmp    da4 <_dl_relocate_object+0xaa4>
     e65:	0f 1f 00             	nopl   (%rax)
     e68:	4c 8b 6d 88          	mov    -0x78(%rbp),%r13
     e6c:	4d 85 ed             	test   %r13,%r13
     e6f:	0f 84 3b fd ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     e75:	49 8b 56 10          	mov    0x10(%r14),%rdx
     e79:	49 8b 45 10          	mov    0x10(%r13),%rax
     e7d:	4c 89 ce             	mov    %r9,%rsi
     e80:	4c 89 d7             	mov    %r10,%rdi
     e83:	48 39 c2             	cmp    %rax,%rdx
     e86:	48 0f 47 d0          	cmova  %rax,%rdx
     e8a:	e8 00 00 00 00       	call   e8f <_dl_relocate_object+0xb8f>	e8b: R_X86_64_PLT32	memcpy-0x4
     e8f:	49 8b 55 10          	mov    0x10(%r13),%rdx
     e93:	49 8b 46 10          	mov    0x10(%r14),%rax
     e97:	48 39 d0             	cmp    %rdx,%rax
     e9a:	72 19                	jb     eb5 <_dl_relocate_object+0xbb5>
     e9c:	48 39 c2             	cmp    %rax,%rdx
     e9f:	0f 83 0b fd ff ff    	jae    bb0 <_dl_relocate_object+0x8b0>
     ea5:	44 8b 05 00 00 00 00 	mov    0x0(%rip),%r8d        # eac <_dl_relocate_object+0xbac>	ea8: R_X86_64_PC32	_dl_verbose-0x4
     eac:	45 85 c0             	test   %r8d,%r8d
     eaf:	0f 84 fb fc ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     eb5:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # ebc <_dl_relocate_object+0xbbc>	eb8: R_X86_64_PC32	.LC7-0x4
     ebc:	e9 e3 fe ff ff       	jmp    da4 <_dl_relocate_object+0xaa4>
     ec1:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
     ec8:	4c 89 95 50 ff ff ff 	mov    %r10,-0xb0(%rbp)
     ecf:	48 8b 43 10          	mov    0x10(%rbx),%rax
     ed3:	49 03 04 24          	add    (%r12),%rax
     ed7:	ff d0                	call   *%rax
     ed9:	4c 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%r10
     ee0:	49 89 02             	mov    %rax,(%r10)
     ee3:	e9 c8 fc ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     ee8:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
     ef0:	48 8b 45 88          	mov    -0x78(%rbp),%rax
     ef4:	48 85 c0             	test   %rax,%rax
     ef7:	0f 84 7a 0a 00 00    	je     1977 <_dl_relocate_object+0x1677>
     efd:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
     f04:	48 8d 4a 01          	lea    0x1(%rdx),%rcx
     f08:	48 83 f9 01          	cmp    $0x1,%rcx
     f0c:	0f 86 cc 15 00 00    	jbe    24de <_dl_relocate_object+0x21de>
     f12:	48 8b 40 08          	mov    0x8(%rax),%rax
     f16:	48 29 d0             	sub    %rdx,%rax
     f19:	48 03 43 10          	add    0x10(%rbx),%rax
     f1d:	49 89 42 08          	mov    %rax,0x8(%r10)
     f21:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # f28 <_dl_relocate_object+0xc28>	f24: R_X86_64_PC32	_dl_tlsdesc_return-0x4
     f28:	49 89 02             	mov    %rax,(%r10)
     f2b:	e9 80 fc ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     f30:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
     f34:	48 8b 43 10          	mov    0x10(%rbx),%rax
     f38:	48 03 42 10          	add    0x10(%rdx),%rax
     f3c:	49 89 02             	mov    %rax,(%r10)
     f3f:	e9 6c fc ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     f44:	0f 1f 40 00          	nopl   0x0(%rax)
     f48:	48 8b 45 88          	mov    -0x78(%rbp),%rax
     f4c:	48 85 c0             	test   %rax,%rax
     f4f:	0f 84 5b fc ff ff    	je     bb0 <_dl_relocate_object+0x8b0>
     f55:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
     f5c:	48 8d 4a 01          	lea    0x1(%rdx),%rcx
     f60:	48 83 f9 01          	cmp    $0x1,%rcx
     f64:	0f 86 a8 15 00 00    	jbe    2512 <_dl_relocate_object+0x2212>
     f6a:	48 8b 40 08          	mov    0x8(%rax),%rax
     f6e:	48 29 d0             	sub    %rdx,%rax
     f71:	48 03 43 10          	add    0x10(%rbx),%rax
     f75:	49 89 02             	mov    %rax,(%r10)
     f78:	e9 33 fc ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     f7d:	0f 1f 00             	nopl   (%rax)
     f80:	4c 89 f2             	mov    %r14,%rdx
     f83:	4d 89 e3             	mov    %r12,%r11
     f86:	0f b7 4a 06          	movzwl 0x6(%rdx),%ecx
     f8a:	66 83 f9 f1          	cmp    $0xfff1,%cx
     f8e:	0f 85 ca fb ff ff    	jne    b5e <_dl_relocate_object+0x85e>
     f94:	45 31 c9             	xor    %r9d,%r9d
     f97:	e9 c5 fb ff ff       	jmp    b61 <_dl_relocate_object+0x861>
     f9c:	0f 1f 40 00          	nopl   0x0(%rax)
     fa0:	48 83 bd 40 ff ff ff 00 	cmpq   $0x0,-0xc0(%rbp)
     fa8:	48 89 9d 38 ff ff ff 	mov    %rbx,-0xc8(%rbp)
     faf:	0f 85 fb fb ff ff    	jne    bb0 <_dl_relocate_object+0x8b0>
     fb5:	48 89 9d 40 ff ff ff 	mov    %rbx,-0xc0(%rbp)
     fbc:	e9 ef fb ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     fc1:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
     fc8:	48 8b 43 10          	mov    0x10(%rbx),%rax
     fcc:	49 03 04 24          	add    (%r12),%rax
     fd0:	49 89 02             	mov    %rax,(%r10)
     fd3:	e9 d8 fb ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
     fd8:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
     fe0:	45 31 c9             	xor    %r9d,%r9d
     fe3:	49 83 fd 24          	cmp    $0x24,%r13
     fe7:	41 0f 94 c1          	sete   %r9b
     feb:	e9 b8 fa ff ff       	jmp    aa8 <_dl_relocate_object+0x7a8>
     ff0:	4c 89 d2             	mov    %r10,%rdx
     ff3:	4c 89 e1             	mov    %r12,%rcx
     ff6:	0f b7 72 06          	movzwl 0x6(%rdx),%esi
     ffa:	66 83 fe f1          	cmp    $0xfff1,%si
     ffe:	0f 85 39 fd ff ff    	jne    d3d <_dl_relocate_object+0xa3d>
    1004:	45 31 db             	xor    %r11d,%r11d
    1007:	e9 34 fd ff ff       	jmp    d40 <_dl_relocate_object+0xa40>
    100c:	49 8b 0b             	mov    (%r11),%rcx
    100f:	49 8b 14 24          	mov    (%r12),%rdx
    1013:	49 83 c3 18          	add    $0x18,%r11
    1017:	49 03 53 f8          	add    -0x8(%r11),%rdx
    101b:	4a 89 14 29          	mov    %rdx,(%rcx,%r13,1)
    101f:	4d 39 df             	cmp    %r11,%r15
    1022:	0f 82 91 f6 ff ff    	jb     6b9 <_dl_relocate_object+0x3b9>
    1028:	41 83 7b 08 25       	cmpl   $0x25,0x8(%r11)
    102d:	74 dd                	je     100c <_dl_relocate_object+0xd0c>
    102f:	49 83 c3 18          	add    $0x18,%r11
    1033:	4d 39 df             	cmp    %r11,%r15
    1036:	73 f0                	jae    1028 <_dl_relocate_object+0xd28>
    1038:	e9 7c f6 ff ff       	jmp    6b9 <_dl_relocate_object+0x3b9>
    103d:	0f 1f 00             	nopl   (%rax)
    1040:	41 8b 84 24 48 04 00 00 	mov    0x448(%r12),%eax
    1048:	49 83 fd 12          	cmp    $0x12,%r13
    104c:	0f 87 3c 09 00 00    	ja     198e <_dl_relocate_object+0x168e>
    1052:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    1059:	75 14                	jne    106f <_dl_relocate_object+0xd6f>
    105b:	49 83 fd 05          	cmp    $0x5,%r13
    105f:	0f 84 52 0b 00 00    	je     1bb7 <_dl_relocate_object+0x18b7>
    1065:	49 83 fd 07          	cmp    $0x7,%r13
    1069:	0f 85 5f 14 00 00    	jne    24ce <_dl_relocate_object+0x21ce>
    106f:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    1075:	83 f8 01             	cmp    $0x1,%eax
    1078:	0f 85 2a fa ff ff    	jne    aa8 <_dl_relocate_object+0x7a8>
    107e:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    1086:	4d 8b 9c 24 50 04 00 00 	mov    0x450(%r12),%r11
    108e:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    1092:	e9 ad fa ff ff       	jmp    b44 <_dl_relocate_object+0x844>
    1097:	66 0f 1f 84 00 00 00 00 00 	nopw   0x0(%rax,%rax,1)
    10a0:	48 89 f2             	mov    %rsi,%rdx
    10a3:	4d 89 e1             	mov    %r12,%r9
    10a6:	0f b7 4a 06          	movzwl 0x6(%rdx),%ecx
    10aa:	66 83 f9 f1          	cmp    $0xfff1,%cx
    10ae:	0f 84 e0 0b 00 00    	je     1c94 <_dl_relocate_object+0x1994>
    10b4:	49 8b 31             	mov    (%r9),%rsi
    10b7:	48 89 b5 68 ff ff ff 	mov    %rsi,-0x98(%rbp)
    10be:	48 8b 72 08          	mov    0x8(%rdx),%rsi
    10c2:	83 e0 0f             	and    $0xf,%eax
    10c5:	3c 0a                	cmp    $0xa,%al
    10c7:	48 89 b5 60 ff ff ff 	mov    %rsi,-0xa0(%rbp)
    10ce:	40 0f 94 c6          	sete   %sil
    10d2:	66 85 c9             	test   %cx,%cx
    10d5:	0f 95 c0             	setne  %al
    10d8:	40 84 c6             	test   %al,%sil
    10db:	74 0e                	je     10eb <_dl_relocate_object+0xdeb>
    10dd:	8b 85 30 ff ff ff    	mov    -0xd0(%rbp),%eax
    10e3:	85 c0                	test   %eax,%eax
    10e5:	0f 84 31 0c 00 00    	je     1d1c <_dl_relocate_object+0x1a1c>
    10eb:	49 8b 89 88 04 00 00 	mov    0x488(%r9),%rcx
    10f2:	48 8d 41 01          	lea    0x1(%rcx),%rax
    10f6:	48 83 f8 01          	cmp    $0x1,%rax
    10fa:	0f 86 52 0b 00 00    	jbe    1c52 <_dl_relocate_object+0x1952>
    1100:	48 8b 42 08          	mov    0x8(%rdx),%rax
    1104:	48 29 c8             	sub    %rcx,%rax
    1107:	48 03 43 10          	add    0x10(%rbx),%rax
    110b:	49 89 46 08          	mov    %rax,0x8(%r14)
    110f:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1116 <_dl_relocate_object+0xe16>	1112: R_X86_64_PC32	_dl_tlsdesc_return-0x4
    1116:	49 89 06             	mov    %rax,(%r14)
    1119:	e9 16 f5 ff ff       	jmp    634 <_dl_relocate_object+0x334>
    111e:	66 90                	xchg   %ax,%ax
    1120:	4d 39 e3             	cmp    %r12,%r11
    1123:	0f 84 90 00 00 00    	je     11b9 <_dl_relocate_object+0xeb9>
    1129:	41 0f b6 83 54 03 00 00 	movzbl 0x354(%r11),%eax
    1131:	a8 08                	test   $0x8,%al
    1133:	0f 85 80 00 00 00    	jne    11b9 <_dl_relocate_object+0xeb9>
    1139:	49 8b 54 24 68       	mov    0x68(%r12),%rdx
    113e:	48 8b 52 08          	mov    0x8(%rdx),%rdx
    1142:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    114b:	74 04                	je     1151 <_dl_relocate_object+0xe51>
    114d:	49 03 14 24          	add    (%r12),%rdx
    1151:	41 8b 0e             	mov    (%r14),%ecx
    1154:	4c 8d 04 0a          	lea    (%rdx,%rcx,1),%r8
    1158:	48 8b 0d 00 00 00 00 	mov    0x0(%rip),%rcx        # 115f <_dl_relocate_object+0xe5f>	115b: R_X86_64_PC32	_dl_argv-0x4
    115f:	49 8b 54 24 08       	mov    0x8(%r12),%rdx
    1164:	48 8b 31             	mov    (%rcx),%rsi
    1167:	a8 03                	test   $0x3,%al
    1169:	0f 84 28 1c 00 00    	je     2d97 <_dl_relocate_object+0x2a97>
    116f:	48 85 f6             	test   %rsi,%rsi
    1172:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1179 <_dl_relocate_object+0xe79>	1175: R_X86_64_PC32	.LC6-0x4
    1179:	49 8b 4b 08          	mov    0x8(%r11),%rcx
    117d:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1184 <_dl_relocate_object+0xe84>	1180: R_X86_64_PC32	.LC12-0x4
    1184:	48 0f 44 f0          	cmove  %rax,%rsi
    1188:	31 c0                	xor    %eax,%eax
    118a:	4c 89 8d 08 ff ff ff 	mov    %r9,-0xf8(%rbp)
    1191:	4c 89 95 10 ff ff ff 	mov    %r10,-0xf0(%rbp)
    1198:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    119f:	e8 00 00 00 00       	call   11a4 <_dl_relocate_object+0xea4>	11a0: R_X86_64_PLT32	_dl_error_printf-0x4
    11a4:	4c 8b 8d 08 ff ff ff 	mov    -0xf8(%rbp),%r9
    11ab:	4c 8b 95 10 ff ff ff 	mov    -0xf0(%rbp),%r10
    11b2:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    11b9:	4c 89 95 10 ff ff ff 	mov    %r10,-0xf0(%rbp)
    11c0:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    11c7:	41 ff d1             	call   *%r9
    11ca:	4c 8b 95 10 ff ff ff 	mov    -0xf0(%rbp),%r10
    11d1:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    11d8:	49 89 c1             	mov    %rax,%r9
    11db:	e9 95 f9 ff ff       	jmp    b75 <_dl_relocate_object+0x875>
    11e0:	0f b6 42 04          	movzbl 0x4(%rdx),%eax
    11e4:	e9 bd fe ff ff       	jmp    10a6 <_dl_relocate_object+0xda6>
    11e9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    11f0:	49 8b 3c 24          	mov    (%r12),%rdi
    11f4:	48 01 fe             	add    %rdi,%rsi
    11f7:	48 89 b5 68 ff ff ff 	mov    %rsi,-0x98(%rbp)
    11fe:	e9 94 f7 ff ff       	jmp    997 <_dl_relocate_object+0x697>
    1203:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    1208:	31 ff                	xor    %edi,%edi
    120a:	31 d2                	xor    %edx,%edx
    120c:	e9 f1 f2 ff ff       	jmp    502 <_dl_relocate_object+0x202>
    1211:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    1218:	48 8b 15 00 00 00 00 	mov    0x0(%rip),%rdx        # 121f <_dl_relocate_object+0xf1f>	121b: R_X86_64_PC32	_dl_pagesize-0x4
    121f:	49 8b 8a a0 04 00 00 	mov    0x4a0(%r10),%rcx
    1226:	4c 89 95 78 ff ff ff 	mov    %r10,-0x88(%rbp)
    122d:	49 03 0a             	add    (%r10),%rcx
    1230:	48 f7 da             	neg    %rdx
    1233:	48 89 cf             	mov    %rcx,%rdi
    1236:	48 01 c8             	add    %rcx,%rax
    1239:	48 21 d7             	and    %rdx,%rdi
    123c:	48 21 d0             	and    %rdx,%rax
    123f:	48 39 c7             	cmp    %rax,%rdi
    1242:	0f 84 25 f5 ff ff    	je     76d <_dl_relocate_object+0x46d>
    1248:	48 29 f8             	sub    %rdi,%rax
    124b:	ba 01 00 00 00       	mov    $0x1,%edx
    1250:	48 89 c6             	mov    %rax,%rsi
    1253:	e8 00 00 00 00       	call   1258 <_dl_relocate_object+0xf58>	1254: R_X86_64_PLT32	__mprotect-0x4
    1258:	4c 8b 95 78 ff ff ff 	mov    -0x88(%rbp),%r10
    125f:	85 c0                	test   %eax,%eax
    1261:	0f 89 06 f5 ff ff    	jns    76d <_dl_relocate_object+0x46d>
    1267:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 126e <_dl_relocate_object+0xf6e>	126a: R_X86_64_GOTTPOFF	__libc_errno-0x4
    126e:	49 8b 72 08          	mov    0x8(%r10),%rsi
    1272:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 1279 <_dl_relocate_object+0xf79>	1275: R_X86_64_PC32	errstring.1-0x4
    1279:	31 d2                	xor    %edx,%edx
    127b:	64 8b 38             	mov    %fs:(%rax),%edi
    127e:	e8 00 00 00 00       	call   1283 <_dl_relocate_object+0xf83>	127f: R_X86_64_PLT32	_dl_signal_error-0x4
    1283:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    1288:	45 31 c0             	xor    %r8d,%r8d
    128b:	e9 08 f6 ff ff       	jmp    898 <_dl_relocate_object+0x598>
    1290:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1297 <_dl_relocate_object+0xf97>	1293: R_X86_64_PC32	_dl_runtime_resolve_fxsave-0x4
    1297:	48 89 78 10          	mov    %rdi,0x10(%rax)
    129b:	e9 40 f1 ff ff       	jmp    3e0 <_dl_relocate_object+0xe0>
    12a0:	49 01 f8             	add    %rdi,%r8
    12a3:	4c 89 45 98          	mov    %r8,-0x68(%rbp)
    12a7:	e9 be f2 ff ff       	jmp    56a <_dl_relocate_object+0x26a>
    12ac:	0f 1f 40 00          	nopl   0x0(%rax)
    12b0:	48 8b bd 70 ff ff ff 	mov    -0x90(%rbp),%rdi
    12b7:	48 39 fb             	cmp    %rdi,%rbx
    12ba:	0f 83 f9 f3 ff ff    	jae    6b9 <_dl_relocate_object+0x3b9>
    12c0:	44 8b 85 30 ff ff ff 	mov    -0xd0(%rbp),%r8d
    12c7:	48 c7 85 40 ff ff ff 00 00 00 00 	movq   $0x0,-0xc0(%rbp)
    12d2:	45 85 c0             	test   %r8d,%r8d
    12d5:	0f 85 7a 12 00 00    	jne    2555 <_dl_relocate_object+0x2255>
    12db:	4c 89 95 50 ff ff ff 	mov    %r10,-0xb0(%rbp)
    12e2:	4c 89 ad 68 ff ff ff 	mov    %r13,-0x98(%rbp)
    12e9:	48 89 85 38 ff ff ff 	mov    %rax,-0xc8(%rbp)
    12f0:	4c 8b 7b 08          	mov    0x8(%rbx),%r15
    12f4:	48 8b bd 78 ff ff ff 	mov    -0x88(%rbp),%rdi
    12fb:	4c 8b 95 68 ff ff ff 	mov    -0x98(%rbp),%r10
    1302:	4c 03 13             	add    (%rbx),%r10
    1305:	4c 89 f8             	mov    %r15,%rax
    1308:	45 89 fd             	mov    %r15d,%r13d
    130b:	48 c1 e8 20          	shr    $0x20,%rax
    130f:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    1313:	4c 8d 34 c7          	lea    (%rdi,%rax,8),%r14
    1317:	41 83 ff 25          	cmp    $0x25,%r15d
    131b:	0f 84 e7 04 00 00    	je     1808 <_dl_relocate_object+0x1508>
    1321:	4c 89 75 88          	mov    %r14,-0x78(%rbp)
    1325:	49 83 fd 08          	cmp    $0x8,%r13
    1329:	0f 84 01 05 00 00    	je     1830 <_dl_relocate_object+0x1530>
    132f:	49 83 fd 26          	cmp    $0x26,%r13
    1333:	0f 84 f7 04 00 00    	je     1830 <_dl_relocate_object+0x1530>
    1339:	4d 85 ed             	test   %r13,%r13
    133c:	0f 84 3e 01 00 00    	je     1480 <_dl_relocate_object+0x1180>
    1342:	41 0f b6 46 04       	movzbl 0x4(%r14),%eax
    1347:	89 c7                	mov    %eax,%edi
    1349:	40 c0 ef 04          	shr    $0x4,%dil
    134d:	0f 84 95 04 00 00    	je     17e8 <_dl_relocate_object+0x14e8>
    1353:	41 0f b6 56 05       	movzbl 0x5(%r14),%edx
    1358:	83 e2 03             	and    $0x3,%edx
    135b:	83 ea 01             	sub    $0x1,%edx
    135e:	83 fa 01             	cmp    $0x1,%edx
    1361:	0f 86 81 04 00 00    	jbe    17e8 <_dl_relocate_object+0x14e8>
    1367:	4d 3b b4 24 40 04 00 00 	cmp    0x440(%r12),%r14
    136f:	0f 84 eb 04 00 00    	je     1860 <_dl_relocate_object+0x1560>
    1375:	49 83 fd 12          	cmp    $0x12,%r13
    1379:	0f 87 c1 04 00 00    	ja     1840 <_dl_relocate_object+0x1540>
    137f:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    1385:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    138c:	75 17                	jne    13a5 <_dl_relocate_object+0x10a5>
    138e:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    1394:	49 83 fd 05          	cmp    $0x5,%r13
    1398:	74 0b                	je     13a5 <_dl_relocate_object+0x10a5>
    139a:	45 31 c9             	xor    %r9d,%r9d
    139d:	49 83 fd 07          	cmp    $0x7,%r13
    13a1:	41 0f 94 c1          	sete   %r9b
    13a5:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    13aa:	41 8b 3e             	mov    (%r14),%edi
    13ad:	45 89 8c 24 48 04 00 00 	mov    %r9d,0x448(%r12)
    13b5:	4d 89 b4 24 40 04 00 00 	mov    %r14,0x440(%r12)
    13bd:	48 8b 50 08          	mov    0x8(%rax),%rdx
    13c1:	31 c0                	xor    %eax,%eax
    13c3:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    13cc:	74 04                	je     13d2 <_dl_relocate_object+0x10d2>
    13ce:	49 8b 04 24          	mov    (%r12),%rax
    13d2:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    13d9:	48 01 d7             	add    %rdx,%rdi
    13dc:	45 31 c0             	xor    %r8d,%r8d
    13df:	4c 89 e6             	mov    %r12,%rsi
    13e2:	6a 00                	push   $0x0
    13e4:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    13eb:	48 01 c7             	add    %rax,%rdi
    13ee:	6a 09                	push   $0x9
    13f0:	48 8b 95 48 ff ff ff 	mov    -0xb8(%rbp),%rdx
    13f7:	e8 00 00 00 00       	call   13fc <_dl_relocate_object+0x10fc>	13f8: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    13fc:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    1400:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    1407:	66 48 0f 6e c0       	movq   %rax,%xmm0
    140c:	49 89 c3             	mov    %rax,%r11
    140f:	66 48 0f 6e d2       	movq   %rdx,%xmm2
    1414:	66 0f 6c c2          	punpcklqdq %xmm2,%xmm0
    1418:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
    1421:	58                   	pop    %rax
    1422:	59                   	pop    %rcx
    1423:	45 31 c9             	xor    %r9d,%r9d
    1426:	48 85 d2             	test   %rdx,%rdx
    1429:	74 29                	je     1454 <_dl_relocate_object+0x1154>
    142b:	0f b7 4a 06          	movzwl 0x6(%rdx),%ecx
    142f:	0f b6 42 04          	movzbl 0x4(%rdx),%eax
    1433:	66 83 f9 f1          	cmp    $0xfff1,%cx
    1437:	0f 84 bf 03 00 00    	je     17fc <_dl_relocate_object+0x14fc>
    143d:	4d 8b 0b             	mov    (%r11),%r9
    1440:	83 e0 0f             	and    $0xf,%eax
    1443:	4c 03 4a 08          	add    0x8(%rdx),%r9
    1447:	3c 0a                	cmp    $0xa,%al
    1449:	75 09                	jne    1454 <_dl_relocate_object+0x1154>
    144b:	66 85 c9             	test   %cx,%cx
    144e:	0f 85 63 04 00 00    	jne    18b7 <_dl_relocate_object+0x15b7>
    1454:	49 83 fd 25          	cmp    $0x25,%r13
    1458:	0f 87 32 f7 ff ff    	ja     b90 <_dl_relocate_object+0x890>
    145e:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 1465 <_dl_relocate_object+0x1165>	1461: R_X86_64_PC32	.rodata+0x94
    1465:	4a 63 04 ae          	movslq (%rsi,%r13,4),%rax
    1469:	48 01 f0             	add    %rsi,%rax
    146c:	3e ff e0             	notrack jmp *%rax
    146f:	4c 03 4b 10          	add    0x10(%rbx),%r9
    1473:	4d 89 0a             	mov    %r9,(%r10)
    1476:	66 2e 0f 1f 84 00 00 00 00 00 	cs nopw 0x0(%rax,%rax,1)
    1480:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
    1487:	48 83 c3 18          	add    $0x18,%rbx
    148b:	48 39 c3             	cmp    %rax,%rbx
    148e:	0f 82 5c fe ff ff    	jb     12f0 <_dl_relocate_object+0xff0>
    1494:	4c 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%r10
    149b:	4c 8b ad 68 ff ff ff 	mov    -0x98(%rbp),%r13
    14a2:	48 8b 85 38 ff ff ff 	mov    -0xc8(%rbp),%rax
    14a9:	4d 85 d2             	test   %r10,%r10
    14ac:	0f 84 07 f2 ff ff    	je     6b9 <_dl_relocate_object+0x3b9>
    14b2:	4c 39 95 40 ff ff ff 	cmp    %r10,-0xc0(%rbp)
    14b9:	0f 82 fa f1 ff ff    	jb     6b9 <_dl_relocate_object+0x3b9>
    14bf:	48 89 85 68 ff ff ff 	mov    %rax,-0x98(%rbp)
    14c6:	4c 8b b5 40 ff ff ff 	mov    -0xc0(%rbp),%r14
    14cd:	4c 89 d3             	mov    %r10,%rbx
    14d0:	eb 13                	jmp    14e5 <_dl_relocate_object+0x11e5>
    14d2:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
    14d8:	48 83 c3 18          	add    $0x18,%rbx
    14dc:	49 39 de             	cmp    %rbx,%r14
    14df:	0f 82 23 01 00 00    	jb     1608 <_dl_relocate_object+0x1308>
    14e5:	48 8b 43 08          	mov    0x8(%rbx),%rax
    14e9:	83 f8 25             	cmp    $0x25,%eax
    14ec:	75 ea                	jne    14d8 <_dl_relocate_object+0x11d8>
    14ee:	48 8b bd 78 ff ff ff 	mov    -0x88(%rbp),%rdi
    14f5:	48 c1 e8 20          	shr    $0x20,%rax
    14f9:	4c 8b 3b             	mov    (%rbx),%r15
    14fc:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    1500:	4c 8d 14 c7          	lea    (%rdi,%rax,8),%r10
    1504:	4d 01 ef             	add    %r13,%r15
    1507:	4c 89 55 88          	mov    %r10,-0x78(%rbp)
    150b:	41 0f b6 42 04       	movzbl 0x4(%r10),%eax
    1510:	89 c7                	mov    %eax,%edi
    1512:	40 c0 ef 04          	shr    $0x4,%dil
    1516:	0f 84 34 03 00 00    	je     1850 <_dl_relocate_object+0x1550>
    151c:	41 0f b6 52 05       	movzbl 0x5(%r10),%edx
    1521:	83 e2 03             	and    $0x3,%edx
    1524:	83 ea 01             	sub    $0x1,%edx
    1527:	83 fa 01             	cmp    $0x1,%edx
    152a:	0f 86 20 03 00 00    	jbe    1850 <_dl_relocate_object+0x1550>
    1530:	4d 3b 94 24 40 04 00 00 	cmp    0x440(%r12),%r10
    1538:	0f 84 73 0e 00 00    	je     23b1 <_dl_relocate_object+0x20b1>
    153e:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    1543:	41 8b 3a             	mov    (%r10),%edi
    1546:	4d 89 94 24 40 04 00 00 	mov    %r10,0x440(%r12)
    154e:	41 c7 84 24 48 04 00 00 00 00 00 00 	movl   $0x0,0x448(%r12)
    155a:	48 8b 48 08          	mov    0x8(%rax),%rcx
    155e:	31 c0                	xor    %eax,%eax
    1560:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    1569:	74 04                	je     156f <_dl_relocate_object+0x126f>
    156b:	49 8b 04 24          	mov    (%r12),%rax
    156f:	4c 89 95 70 ff ff ff 	mov    %r10,-0x90(%rbp)
    1576:	48 01 cf             	add    %rcx,%rdi
    1579:	4c 89 e6             	mov    %r12,%rsi
    157c:	45 31 c9             	xor    %r9d,%r9d
    157f:	6a 00                	push   $0x0
    1581:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    1588:	48 8d 55 88          	lea    -0x78(%rbp),%rdx
    158c:	48 01 c7             	add    %rax,%rdi
    158f:	6a 09                	push   $0x9
    1591:	45 31 c0             	xor    %r8d,%r8d
    1594:	e8 00 00 00 00       	call   1599 <_dl_relocate_object+0x1299>	1595: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    1599:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    159d:	4c 8b 95 70 ff ff ff 	mov    -0x90(%rbp),%r10
    15a4:	66 48 0f 6e c0       	movq   %rax,%xmm0
    15a9:	48 89 c1             	mov    %rax,%rcx
    15ac:	66 48 0f 6e fa       	movq   %rdx,%xmm7
    15b1:	66 0f 6c c7          	punpcklqdq %xmm7,%xmm0
    15b5:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
    15be:	5e                   	pop    %rsi
    15bf:	5f                   	pop    %rdi
    15c0:	48 85 d2             	test   %rdx,%rdx
    15c3:	74 29                	je     15ee <_dl_relocate_object+0x12ee>
    15c5:	0f b6 42 04          	movzbl 0x4(%rdx),%eax
    15c9:	0f b7 72 06          	movzwl 0x6(%rdx),%esi
    15cd:	66 83 fe f1          	cmp    $0xfff1,%si
    15d1:	0f 84 3d 07 00 00    	je     1d14 <_dl_relocate_object+0x1a14>
    15d7:	4c 8b 09             	mov    (%rcx),%r9
    15da:	83 e0 0f             	and    $0xf,%eax
    15dd:	4c 8b 5a 08          	mov    0x8(%rdx),%r11
    15e1:	3c 0a                	cmp    $0xa,%al
    15e3:	75 09                	jne    15ee <_dl_relocate_object+0x12ee>
    15e5:	66 85 f6             	test   %si,%si
    15e8:	0f 85 44 0e 00 00    	jne    2432 <_dl_relocate_object+0x2132>
    15ee:	48 8b 43 10          	mov    0x10(%rbx),%rax
    15f2:	49 03 04 24          	add    (%r12),%rax
    15f6:	ff d0                	call   *%rax
    15f8:	48 83 c3 18          	add    $0x18,%rbx
    15fc:	49 89 07             	mov    %rax,(%r15)
    15ff:	49 39 de             	cmp    %rbx,%r14
    1602:	0f 83 dd fe ff ff    	jae    14e5 <_dl_relocate_object+0x11e5>
    1608:	48 8b 85 68 ff ff ff 	mov    -0x98(%rbp),%rax
    160f:	e9 a5 f0 ff ff       	jmp    6b9 <_dl_relocate_object+0x3b9>
    1614:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    1618:	4c 8b 48 10          	mov    0x10(%rax),%r9
    161c:	4c 03 4b 10          	add    0x10(%rbx),%r9
    1620:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
    1625:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 162c <_dl_relocate_object+0x132c>	1628: R_X86_64_PC32	.LC8-0x4
    162c:	45 89 0a             	mov    %r9d,(%r10)
    162f:	4c 39 c8             	cmp    %r9,%rax
    1632:	0f 83 48 fe ff ff    	jae    1480 <_dl_relocate_object+0x1180>
    1638:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    163d:	48 8b 40 08          	mov    0x8(%rax),%rax
    1641:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    164a:	74 04                	je     1650 <_dl_relocate_object+0x1350>
    164c:	49 03 04 24          	add    (%r12),%rax
    1650:	41 8b 16             	mov    (%r14),%edx
    1653:	48 01 c2             	add    %rax,%rdx
    1656:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 165d <_dl_relocate_object+0x135d>	1659: R_X86_64_PC32	_dl_argv-0x4
    165d:	48 8b 30             	mov    (%rax),%rsi
    1660:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1667 <_dl_relocate_object+0x1367>	1663: R_X86_64_PC32	.LC6-0x4
    1667:	48 85 f6             	test   %rsi,%rsi
    166a:	48 0f 44 f0          	cmove  %rax,%rsi
    166e:	31 c0                	xor    %eax,%eax
    1670:	e8 00 00 00 00       	call   1675 <_dl_relocate_object+0x1375>	1671: R_X86_64_PLT32	_dl_error_printf-0x4
    1675:	e9 06 fe ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    167a:	41 c6 84 24 59 03 00 00 01 	movb   $0x1,0x359(%r12)
    1683:	e9 eb fd ff ff       	jmp    1473 <_dl_relocate_object+0x1173>
    1688:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    168c:	48 85 d2             	test   %rdx,%rdx
    168f:	0f 84 eb fd ff ff    	je     1480 <_dl_relocate_object+0x1180>
    1695:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1699:	48 03 42 08          	add    0x8(%rdx),%rax
    169d:	49 89 02             	mov    %rax,(%r10)
    16a0:	e9 db fd ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    16a5:	4d 85 db             	test   %r11,%r11
    16a8:	0f 84 d2 fd ff ff    	je     1480 <_dl_relocate_object+0x1180>
    16ae:	49 8b 83 90 04 00 00 	mov    0x490(%r11),%rax
    16b5:	49 89 02             	mov    %rax,(%r10)
    16b8:	e9 c3 fd ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    16bd:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    16c1:	48 85 c0             	test   %rax,%rax
    16c4:	0f 84 da 05 00 00    	je     1ca4 <_dl_relocate_object+0x19a4>
    16ca:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    16d1:	48 8d 4a 01          	lea    0x1(%rdx),%rcx
    16d5:	48 83 f9 01          	cmp    $0x1,%rcx
    16d9:	0f 86 3e 14 00 00    	jbe    2b1d <_dl_relocate_object+0x281d>
    16df:	48 8b 40 08          	mov    0x8(%rax),%rax
    16e3:	48 29 d0             	sub    %rdx,%rax
    16e6:	48 03 43 10          	add    0x10(%rbx),%rax
    16ea:	49 89 42 08          	mov    %rax,0x8(%r10)
    16ee:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 16f5 <_dl_relocate_object+0x13f5>	16f1: R_X86_64_PC32	_dl_tlsdesc_return-0x4
    16f5:	49 89 02             	mov    %rax,(%r10)
    16f8:	e9 83 fd ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    16fd:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    1701:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1705:	48 03 42 10          	add    0x10(%rdx),%rax
    1709:	49 89 02             	mov    %rax,(%r10)
    170c:	e9 6f fd ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    1711:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    1715:	48 85 c0             	test   %rax,%rax
    1718:	0f 84 62 fd ff ff    	je     1480 <_dl_relocate_object+0x1180>
    171e:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    1725:	48 8d 4a 01          	lea    0x1(%rdx),%rcx
    1729:	48 83 f9 01          	cmp    $0x1,%rcx
    172d:	0f 86 b6 13 00 00    	jbe    2ae9 <_dl_relocate_object+0x27e9>
    1733:	48 8b 40 08          	mov    0x8(%rax),%rax
    1737:	48 29 d0             	sub    %rdx,%rax
    173a:	48 03 43 10          	add    0x10(%rbx),%rax
    173e:	49 89 02             	mov    %rax,(%r10)
    1741:	e9 3a fd ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    1746:	48 8b 43 10          	mov    0x10(%rbx),%rax
    174a:	4c 29 d0             	sub    %r10,%rax
    174d:	4c 01 c8             	add    %r9,%rax
    1750:	48 63 d0             	movslq %eax,%rdx
    1753:	41 89 02             	mov    %eax,(%r10)
    1756:	48 39 d0             	cmp    %rdx,%rax
    1759:	0f 84 21 fd ff ff    	je     1480 <_dl_relocate_object+0x1180>
    175f:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1766 <_dl_relocate_object+0x1466>	1762: R_X86_64_PC32	.LC9-0x4
    1766:	e9 cd fe ff ff       	jmp    1638 <_dl_relocate_object+0x1338>
    176b:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    1772:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1776:	49 03 04 24          	add    (%r12),%rax
    177a:	ff d0                	call   *%rax
    177c:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    1783:	49 89 02             	mov    %rax,(%r10)
    1786:	e9 f5 fc ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    178b:	4c 8b 6d 88          	mov    -0x78(%rbp),%r13
    178f:	4d 85 ed             	test   %r13,%r13
    1792:	0f 84 e8 fc ff ff    	je     1480 <_dl_relocate_object+0x1180>
    1798:	49 8b 46 10          	mov    0x10(%r14),%rax
    179c:	49 8b 55 10          	mov    0x10(%r13),%rdx
    17a0:	4c 89 ce             	mov    %r9,%rsi
    17a3:	4c 89 d7             	mov    %r10,%rdi
    17a6:	48 39 d0             	cmp    %rdx,%rax
    17a9:	48 0f 46 d0          	cmovbe %rax,%rdx
    17ad:	e8 00 00 00 00       	call   17b2 <_dl_relocate_object+0x14b2>	17ae: R_X86_64_PLT32	memcpy-0x4
    17b2:	49 8b 55 10          	mov    0x10(%r13),%rdx
    17b6:	49 8b 46 10          	mov    0x10(%r14),%rax
    17ba:	48 39 d0             	cmp    %rdx,%rax
    17bd:	72 19                	jb     17d8 <_dl_relocate_object+0x14d8>
    17bf:	48 39 c2             	cmp    %rax,%rdx
    17c2:	0f 83 b8 fc ff ff    	jae    1480 <_dl_relocate_object+0x1180>
    17c8:	44 8b 2d 00 00 00 00 	mov    0x0(%rip),%r13d        # 17cf <_dl_relocate_object+0x14cf>	17cb: R_X86_64_PC32	_dl_verbose-0x4
    17cf:	45 85 ed             	test   %r13d,%r13d
    17d2:	0f 84 a8 fc ff ff    	je     1480 <_dl_relocate_object+0x1180>
    17d8:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 17df <_dl_relocate_object+0x14df>	17db: R_X86_64_PC32	.LC7-0x4
    17df:	e9 54 fe ff ff       	jmp    1638 <_dl_relocate_object+0x1338>
    17e4:	0f 1f 40 00          	nopl   0x0(%rax)
    17e8:	4c 89 f2             	mov    %r14,%rdx
    17eb:	4d 89 e3             	mov    %r12,%r11
    17ee:	0f b7 4a 06          	movzwl 0x6(%rdx),%ecx
    17f2:	66 83 f9 f1          	cmp    $0xfff1,%cx
    17f6:	0f 85 41 fc ff ff    	jne    143d <_dl_relocate_object+0x113d>
    17fc:	45 31 c9             	xor    %r9d,%r9d
    17ff:	e9 3c fc ff ff       	jmp    1440 <_dl_relocate_object+0x1140>
    1804:	0f 1f 40 00          	nopl   0x0(%rax)
    1808:	48 83 bd 50 ff ff ff 00 	cmpq   $0x0,-0xb0(%rbp)
    1810:	48 89 9d 40 ff ff ff 	mov    %rbx,-0xc0(%rbp)
    1817:	0f 85 63 fc ff ff    	jne    1480 <_dl_relocate_object+0x1180>
    181d:	48 89 9d 50 ff ff ff 	mov    %rbx,-0xb0(%rbp)
    1824:	e9 57 fc ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    1829:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    1830:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1834:	49 03 04 24          	add    (%r12),%rax
    1838:	49 89 02             	mov    %rax,(%r10)
    183b:	e9 40 fc ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    1840:	45 31 c9             	xor    %r9d,%r9d
    1843:	49 83 fd 24          	cmp    $0x24,%r13
    1847:	41 0f 94 c1          	sete   %r9b
    184b:	e9 55 fb ff ff       	jmp    13a5 <_dl_relocate_object+0x10a5>
    1850:	4c 89 e1             	mov    %r12,%rcx
    1853:	4c 89 d2             	mov    %r10,%rdx
    1856:	e9 6e fd ff ff       	jmp    15c9 <_dl_relocate_object+0x12c9>
    185b:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    1860:	41 8b 84 24 48 04 00 00 	mov    0x448(%r12),%eax
    1868:	49 83 fd 12          	cmp    $0x12,%r13
    186c:	0f 87 9e 05 00 00    	ja     1e10 <_dl_relocate_object+0x1b10>
    1872:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    1879:	0f 85 61 01 00 00    	jne    19e0 <_dl_relocate_object+0x16e0>
    187f:	49 83 fd 05          	cmp    $0x5,%r13
    1883:	0f 84 4f 04 00 00    	je     1cd8 <_dl_relocate_object+0x19d8>
    1889:	49 83 fd 07          	cmp    $0x7,%r13
    188d:	0f 84 4d 01 00 00    	je     19e0 <_dl_relocate_object+0x16e0>
    1893:	45 31 c9             	xor    %r9d,%r9d
    1896:	85 c0                	test   %eax,%eax
    1898:	0f 85 07 fb ff ff    	jne    13a5 <_dl_relocate_object+0x10a5>
    189e:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    18a6:	4d 8b 9c 24 50 04 00 00 	mov    0x450(%r12),%r11
    18ae:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    18b2:	e9 6c fb ff ff       	jmp    1423 <_dl_relocate_object+0x1123>
    18b7:	4d 39 dc             	cmp    %r11,%r12
    18ba:	0f 84 90 00 00 00    	je     1950 <_dl_relocate_object+0x1650>
    18c0:	41 0f b6 83 54 03 00 00 	movzbl 0x354(%r11),%eax
    18c8:	a8 08                	test   $0x8,%al
    18ca:	0f 85 80 00 00 00    	jne    1950 <_dl_relocate_object+0x1650>
    18d0:	49 8b 54 24 68       	mov    0x68(%r12),%rdx
    18d5:	48 8b 52 08          	mov    0x8(%rdx),%rdx
    18d9:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    18e2:	74 04                	je     18e8 <_dl_relocate_object+0x15e8>
    18e4:	49 03 14 24          	add    (%r12),%rdx
    18e8:	41 8b 0e             	mov    (%r14),%ecx
    18eb:	4c 8d 04 0a          	lea    (%rdx,%rcx,1),%r8
    18ef:	48 8b 0d 00 00 00 00 	mov    0x0(%rip),%rcx        # 18f6 <_dl_relocate_object+0x15f6>	18f2: R_X86_64_PC32	_dl_argv-0x4
    18f6:	49 8b 54 24 08       	mov    0x8(%r12),%rdx
    18fb:	48 8b 31             	mov    (%rcx),%rsi
    18fe:	a8 03                	test   $0x3,%al
    1900:	0f 84 91 14 00 00    	je     2d97 <_dl_relocate_object+0x2a97>
    1906:	48 85 f6             	test   %rsi,%rsi
    1909:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1910 <_dl_relocate_object+0x1610>	190c: R_X86_64_PC32	.LC6-0x4
    1910:	49 8b 4b 08          	mov    0x8(%r11),%rcx
    1914:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 191b <_dl_relocate_object+0x161b>	1917: R_X86_64_PC32	.LC12-0x4
    191b:	48 0f 44 f0          	cmove  %rax,%rsi
    191f:	31 c0                	xor    %eax,%eax
    1921:	4c 89 8d 10 ff ff ff 	mov    %r9,-0xf0(%rbp)
    1928:	4c 89 95 18 ff ff ff 	mov    %r10,-0xe8(%rbp)
    192f:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    1936:	e8 00 00 00 00       	call   193b <_dl_relocate_object+0x163b>	1937: R_X86_64_PLT32	_dl_error_printf-0x4
    193b:	4c 8b 8d 10 ff ff ff 	mov    -0xf0(%rbp),%r9
    1942:	4c 8b 95 18 ff ff ff 	mov    -0xe8(%rbp),%r10
    1949:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    1950:	4c 89 95 18 ff ff ff 	mov    %r10,-0xe8(%rbp)
    1957:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    195e:	41 ff d1             	call   *%r9
    1961:	4c 8b 95 18 ff ff ff 	mov    -0xe8(%rbp),%r10
    1968:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    196f:	49 89 c1             	mov    %rax,%r9
    1972:	e9 dd fa ff ff       	jmp    1454 <_dl_relocate_object+0x1154>
    1977:	48 8b 43 10          	mov    0x10(%rbx),%rax
    197b:	49 89 42 08          	mov    %rax,0x8(%r10)
    197f:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1986 <_dl_relocate_object+0x1686>	1982: R_X86_64_PC32	_dl_tlsdesc_undefweak-0x4
    1986:	49 89 02             	mov    %rax,(%r10)
    1989:	e9 22 f2 ff ff       	jmp    bb0 <_dl_relocate_object+0x8b0>
    198e:	49 83 fd 24          	cmp    $0x24,%r13
    1992:	0f 84 d7 f6 ff ff    	je     106f <_dl_relocate_object+0xd6f>
    1998:	45 31 c9             	xor    %r9d,%r9d
    199b:	85 c0                	test   %eax,%eax
    199d:	0f 85 05 f1 ff ff    	jne    aa8 <_dl_relocate_object+0x7a8>
    19a3:	e9 d6 f6 ff ff       	jmp    107e <_dl_relocate_object+0xd7e>
    19a8:	41 8b 84 24 48 04 00 00 	mov    0x448(%r12),%eax
    19b0:	85 c0                	test   %eax,%eax
    19b2:	0f 85 c9 f2 ff ff    	jne    c81 <_dl_relocate_object+0x981>
    19b8:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    19c0:	49 8b 8c 24 50 04 00 00 	mov    0x450(%r12),%rcx
    19c8:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    19cc:	e9 55 f3 ff ff       	jmp    d26 <_dl_relocate_object+0xa26>
    19d1:	ba 01 00 00 00       	mov    $0x1,%edx
    19d6:	89 ce                	mov    %ecx,%esi
    19d8:	4c 89 e7             	mov    %r12,%rdi
    19db:	e8 00 00 00 00       	call   19e0 <_dl_relocate_object+0x16e0>	19dc: R_X86_64_PLT32	_dl_reloc_bad_type-0x4
    19e0:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    19e6:	83 f8 01             	cmp    $0x1,%eax
    19e9:	0f 85 b6 f9 ff ff    	jne    13a5 <_dl_relocate_object+0x10a5>
    19ef:	e9 aa fe ff ff       	jmp    189e <_dl_relocate_object+0x159e>
    19f4:	49 39 cc             	cmp    %rcx,%r12
    19f7:	0f 84 80 00 00 00    	je     1a7d <_dl_relocate_object+0x177d>
    19fd:	0f b6 b9 54 03 00 00 	movzbl 0x354(%rcx),%edi
    1a04:	40 f6 c7 08          	test   $0x8,%dil
    1a08:	75 73                	jne    1a7d <_dl_relocate_object+0x177d>
    1a0a:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    1a0f:	48 8b 40 08          	mov    0x8(%rax),%rax
    1a13:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    1a1c:	74 04                	je     1a22 <_dl_relocate_object+0x1722>
    1a1e:	49 03 04 24          	add    (%r12),%rax
    1a22:	41 8b 12             	mov    (%r10),%edx
    1a25:	83 e7 03             	and    $0x3,%edi
    1a28:	4c 8d 04 10          	lea    (%rax,%rdx,1),%r8
    1a2c:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 1a33 <_dl_relocate_object+0x1733>	1a2f: R_X86_64_PC32	_dl_argv-0x4
    1a33:	49 8b 54 24 08       	mov    0x8(%r12),%rdx
    1a38:	48 8b 30             	mov    (%rax),%rsi
    1a3b:	0f 84 56 13 00 00    	je     2d97 <_dl_relocate_object+0x2a97>
    1a41:	48 85 f6             	test   %rsi,%rsi
    1a44:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1a4b <_dl_relocate_object+0x174b>	1a47: R_X86_64_PC32	.LC6-0x4
    1a4b:	48 8b 49 08          	mov    0x8(%rcx),%rcx
    1a4f:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1a56 <_dl_relocate_object+0x1756>	1a52: R_X86_64_PC32	.LC12-0x4
    1a56:	48 0f 44 f0          	cmove  %rax,%rsi
    1a5a:	31 c0                	xor    %eax,%eax
    1a5c:	4c 89 8d 50 ff ff ff 	mov    %r9,-0xb0(%rbp)
    1a63:	4c 89 9d 70 ff ff ff 	mov    %r11,-0x90(%rbp)
    1a6a:	e8 00 00 00 00       	call   1a6f <_dl_relocate_object+0x176f>	1a6b: R_X86_64_PLT32	_dl_error_printf-0x4
    1a6f:	4c 8b 8d 50 ff ff ff 	mov    -0xb0(%rbp),%r9
    1a76:	4c 8b 9d 70 ff ff ff 	mov    -0x90(%rbp),%r11
    1a7d:	4d 01 d9             	add    %r11,%r9
    1a80:	41 ff d1             	call   *%r9
    1a83:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1a87:	49 03 04 24          	add    (%r12),%rax
    1a8b:	e9 cc f2 ff ff       	jmp    d5c <_dl_relocate_object+0xa5c>
    1a90:	45 31 ed             	xor    %r13d,%r13d
    1a93:	41 0f b7 92 f0 02 00 00 	movzwl 0x2f0(%r10),%edx
    1a9b:	49 8b b2 e0 02 00 00 	mov    0x2e0(%r10),%rsi
    1aa2:	48 8d 04 d5 00 00 00 00 	lea    0x0(,%rdx,8),%rax
    1aaa:	48 29 d0             	sub    %rdx,%rax
    1aad:	48 8d 04 c6          	lea    (%rsi,%rax,8),%rax
    1ab1:	48 39 c6             	cmp    %rax,%rsi
    1ab4:	0f 83 30 11 00 00    	jae    2bea <_dl_relocate_object+0x28ea>
    1aba:	31 db                	xor    %ebx,%ebx
    1abc:	44 89 a5 70 ff ff ff 	mov    %r12d,-0x90(%rbp)
    1ac3:	49 89 f7             	mov    %rsi,%r15
    1ac6:	49 be ff ff ff ff 02 00 00 00 	movabs $0x2ffffffff,%r14
    1ad0:	49 89 dc             	mov    %rbx,%r12
    1ad3:	44 89 ad 78 ff ff ff 	mov    %r13d,-0x88(%rbp)
    1ada:	4c 89 d3             	mov    %r10,%rbx
    1add:	eb 1d                	jmp    1afc <_dl_relocate_object+0x17fc>
    1adf:	90                   	nop
    1ae0:	48 8d 04 d5 00 00 00 00 	lea    0x0(,%rdx,8),%rax
    1ae8:	49 83 c7 38          	add    $0x38,%r15
    1aec:	48 29 d0             	sub    %rdx,%rax
    1aef:	48 8d 04 c6          	lea    (%rsi,%rax,8),%rax
    1af3:	49 39 c7             	cmp    %rax,%r15
    1af6:	0f 83 12 01 00 00    	jae    1c0e <_dl_relocate_object+0x190e>
    1afc:	49 8b 07             	mov    (%r15),%rax
    1aff:	4c 21 f0             	and    %r14,%rax
    1b02:	48 83 f8 01          	cmp    $0x1,%rax
    1b06:	75 d8                	jne    1ae0 <_dl_relocate_object+0x17e0>
    1b08:	48 89 e0             	mov    %rsp,%rax
    1b0b:	48 39 c4             	cmp    %rax,%rsp
    1b0e:	74 15                	je     1b25 <_dl_relocate_object+0x1825>
    1b10:	48 81 ec 00 10 00 00 	sub    $0x1000,%rsp
    1b17:	48 83 8c 24 f8 0f 00 00 00 	orq    $0x0,0xff8(%rsp)
    1b20:	48 39 c4             	cmp    %rax,%rsp
    1b23:	75 eb                	jne    1b10 <_dl_relocate_object+0x1810>
    1b25:	48 83 ec 30          	sub    $0x30,%rsp
    1b29:	48 83 4c 24 28 00    	orq    $0x0,0x28(%rsp)
    1b2f:	48 8b 15 00 00 00 00 	mov    0x0(%rip),%rdx        # 1b36 <_dl_relocate_object+0x1836>	1b32: R_X86_64_PC32	_dl_pagesize-0x4
    1b36:	49 8b 77 10          	mov    0x10(%r15),%rsi
    1b3a:	48 89 d0             	mov    %rdx,%rax
    1b3d:	48 f7 d8             	neg    %rax
    1b40:	48 89 f7             	mov    %rsi,%rdi
    1b43:	48 8d 74 16 ff       	lea    -0x1(%rsi,%rdx,1),%rsi
    1b48:	49 03 77 28          	add    0x28(%r15),%rsi
    1b4c:	48 21 c7             	and    %rax,%rdi
    1b4f:	48 21 c6             	and    %rax,%rsi
    1b52:	41 8b 47 04          	mov    0x4(%r15),%eax
    1b56:	4c 8d 44 24 0f       	lea    0xf(%rsp),%r8
    1b5b:	48 29 fe             	sub    %rdi,%rsi
    1b5e:	48 03 3b             	add    (%rbx),%rdi
    1b61:	89 c2                	mov    %eax,%edx
    1b63:	49 83 e0 f0          	and    $0xfffffffffffffff0,%r8
    1b67:	c1 ea 02             	shr    $0x2,%edx
    1b6a:	49 89 70 08          	mov    %rsi,0x8(%r8)
    1b6e:	4d 89 c5             	mov    %r8,%r13
    1b71:	49 89 38             	mov    %rdi,(%r8)
    1b74:	83 e2 01             	and    $0x1,%edx
    1b77:	a8 02                	test   $0x2,%al
    1b79:	74 03                	je     1b7e <_dl_relocate_object+0x187e>
    1b7b:	83 ca 02             	or     $0x2,%edx
    1b7e:	41 89 55 10          	mov    %edx,0x10(%r13)
    1b82:	a8 01                	test   $0x1,%al
    1b84:	74 07                	je     1b8d <_dl_relocate_object+0x188d>
    1b86:	83 ca 04             	or     $0x4,%edx
    1b89:	41 89 55 10          	mov    %edx,0x10(%r13)
    1b8d:	83 ca 02             	or     $0x2,%edx
    1b90:	e8 00 00 00 00       	call   1b95 <_dl_relocate_object+0x1895>	1b91: R_X86_64_PLT32	__mprotect-0x4
    1b95:	85 c0                	test   %eax,%eax
    1b97:	0f 88 cd 11 00 00    	js     2d6a <_dl_relocate_object+0x2a6a>
    1b9d:	4d 89 65 18          	mov    %r12,0x18(%r13)
    1ba1:	48 8b b3 e0 02 00 00 	mov    0x2e0(%rbx),%rsi
    1ba8:	4d 89 ec             	mov    %r13,%r12
    1bab:	0f b7 93 f0 02 00 00 	movzwl 0x2f0(%rbx),%edx
    1bb2:	e9 29 ff ff ff       	jmp    1ae0 <_dl_relocate_object+0x17e0>
    1bb7:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    1bbd:	83 f8 02             	cmp    $0x2,%eax
    1bc0:	0f 85 e2 ee ff ff    	jne    aa8 <_dl_relocate_object+0x7a8>
    1bc6:	e9 b3 f4 ff ff       	jmp    107e <_dl_relocate_object+0xd7e>
    1bcb:	45 85 ed             	test   %r13d,%r13d
    1bce:	48 8d 15 00 00 00 00 	lea    0x0(%rip),%rdx        # 1bd5 <_dl_relocate_object+0x18d5>	1bd1: R_X86_64_PC32	.LC1-0x4
    1bd5:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1bdc <_dl_relocate_object+0x18dc>	1bd8: R_X86_64_PC32	.LC2-0x4
    1bdc:	48 0f 44 d0          	cmove  %rax,%rdx
    1be0:	49 8b 72 08          	mov    0x8(%r10),%rsi
    1be4:	80 3e 00             	cmpb   $0x0,(%rsi)
    1be7:	0f 84 ce 00 00 00    	je     1cbb <_dl_relocate_object+0x19bb>
    1bed:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1bf4 <_dl_relocate_object+0x18f4>	1bf0: R_X86_64_PC32	.LC10-0x4
    1bf4:	31 c0                	xor    %eax,%eax
    1bf6:	4c 89 95 78 ff ff ff 	mov    %r10,-0x88(%rbp)
    1bfd:	e8 00 00 00 00       	call   1c02 <_dl_relocate_object+0x1902>	1bfe: R_X86_64_PLT32	_dl_debug_printf-0x4
    1c02:	4c 8b 95 78 ff ff ff 	mov    -0x88(%rbp),%r10
    1c09:	e9 4a e7 ff ff       	jmp    358 <_dl_relocate_object+0x58>
    1c0e:	49 89 da             	mov    %rbx,%r10
    1c11:	44 8b ad 78 ff ff ff 	mov    -0x88(%rbp),%r13d
    1c18:	4c 89 e3             	mov    %r12,%rbx
    1c1b:	44 8b a5 70 ff ff ff 	mov    -0x90(%rbp),%r12d
    1c22:	e9 41 e7 ff ff       	jmp    368 <_dl_relocate_object+0x68>
    1c27:	4d 89 d4             	mov    %r10,%r12
    1c2a:	8b 53 10             	mov    0x10(%rbx),%edx
    1c2d:	48 8b 73 08          	mov    0x8(%rbx),%rsi
    1c31:	48 8b 3b             	mov    (%rbx),%rdi
    1c34:	e8 00 00 00 00       	call   1c39 <_dl_relocate_object+0x1939>	1c35: R_X86_64_PLT32	__mprotect-0x4
    1c39:	85 c0                	test   %eax,%eax
    1c3b:	0f 88 0a 11 00 00    	js     2d4b <_dl_relocate_object+0x2a4b>
    1c41:	48 8b 5b 18          	mov    0x18(%rbx),%rbx
    1c45:	48 85 db             	test   %rbx,%rbx
    1c48:	75 e0                	jne    1c2a <_dl_relocate_object+0x192a>
    1c4a:	4d 89 e2             	mov    %r12,%r10
    1c4d:	e9 0b eb ff ff       	jmp    75d <_dl_relocate_object+0x45d>
    1c52:	4c 89 cf             	mov    %r9,%rdi
    1c55:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    1c5c:	4c 89 9d 68 ff ff ff 	mov    %r11,-0x98(%rbp)
    1c63:	4c 89 8d 78 ff ff ff 	mov    %r9,-0x88(%rbp)
    1c6a:	e8 00 00 00 00       	call   1c6f <_dl_relocate_object+0x196f>	1c6b: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    1c6f:	4c 8b 8d 78 ff ff ff 	mov    -0x88(%rbp),%r9
    1c76:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    1c7a:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    1c81:	4c 8b 9d 68 ff ff ff 	mov    -0x98(%rbp),%r11
    1c88:	49 8b 89 88 04 00 00 	mov    0x488(%r9),%rcx
    1c8f:	e9 6c f4 ff ff       	jmp    1100 <_dl_relocate_object+0xe00>
    1c94:	48 c7 85 68 ff ff ff 00 00 00 00 	movq   $0x0,-0x98(%rbp)
    1c9f:	e9 1a f4 ff ff       	jmp    10be <_dl_relocate_object+0xdbe>
    1ca4:	48 8b 43 10          	mov    0x10(%rbx),%rax
    1ca8:	49 89 42 08          	mov    %rax,0x8(%r10)
    1cac:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1cb3 <_dl_relocate_object+0x19b3>	1caf: R_X86_64_PC32	_dl_tlsdesc_undefweak-0x4
    1cb3:	49 89 02             	mov    %rax,(%r10)
    1cb6:	e9 c5 f7 ff ff       	jmp    1480 <_dl_relocate_object+0x1180>
    1cbb:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 1cc2 <_dl_relocate_object+0x19c2>	1cbe: R_X86_64_PC32	_dl_argv-0x4
    1cc2:	48 8b 30             	mov    (%rax),%rsi
    1cc5:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1ccc <_dl_relocate_object+0x19cc>	1cc8: R_X86_64_PC32	.LC3-0x4
    1ccc:	48 85 f6             	test   %rsi,%rsi
    1ccf:	48 0f 44 f0          	cmove  %rax,%rsi
    1cd3:	e9 15 ff ff ff       	jmp    1bed <_dl_relocate_object+0x18ed>
    1cd8:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    1cde:	83 f8 02             	cmp    $0x2,%eax
    1ce1:	0f 85 be f6 ff ff    	jne    13a5 <_dl_relocate_object+0x10a5>
    1ce7:	e9 b2 fb ff ff       	jmp    189e <_dl_relocate_object+0x159e>
    1cec:	41 83 bc 24 48 04 00 00 01 	cmpl   $0x1,0x448(%r12)
    1cf5:	0f 85 62 eb ff ff    	jne    85d <_dl_relocate_object+0x55d>
    1cfb:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    1d03:	4d 8b 8c 24 50 04 00 00 	mov    0x450(%r12),%r9
    1d0b:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    1d0f:	e9 dd eb ff ff       	jmp    8f1 <_dl_relocate_object+0x5f1>
    1d14:	45 31 c9             	xor    %r9d,%r9d
    1d17:	e9 be f8 ff ff       	jmp    15da <_dl_relocate_object+0x12da>
    1d1c:	4d 39 cc             	cmp    %r9,%r12
    1d1f:	0f 84 95 00 00 00    	je     1dba <_dl_relocate_object+0x1aba>
    1d25:	41 0f b6 81 54 03 00 00 	movzbl 0x354(%r9),%eax
    1d2d:	a8 08                	test   $0x8,%al
    1d2f:	0f 85 85 00 00 00    	jne    1dba <_dl_relocate_object+0x1aba>
    1d35:	49 8b 54 24 68       	mov    0x68(%r12),%rdx
    1d3a:	4c 8b 42 08          	mov    0x8(%rdx),%r8
    1d3e:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    1d47:	74 04                	je     1d4d <_dl_relocate_object+0x1a4d>
    1d49:	4d 03 04 24          	add    (%r12),%r8
    1d4d:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
    1d54:	48 8b 0d 00 00 00 00 	mov    0x0(%rip),%rcx        # 1d5b <_dl_relocate_object+0x1a5b>	1d57: R_X86_64_PC32	_dl_argv-0x4
    1d5b:	8b 16                	mov    (%rsi),%edx
    1d5d:	48 8b 31             	mov    (%rcx),%rsi
    1d60:	49 01 d0             	add    %rdx,%r8
    1d63:	49 8b 54 24 08       	mov    0x8(%r12),%rdx
    1d68:	a8 03                	test   $0x3,%al
    1d6a:	0f 84 62 10 00 00    	je     2dd2 <_dl_relocate_object+0x2ad2>
    1d70:	48 85 f6             	test   %rsi,%rsi
    1d73:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 1d7a <_dl_relocate_object+0x1a7a>	1d76: R_X86_64_PC32	.LC6-0x4
    1d7a:	49 8b 49 08          	mov    0x8(%r9),%rcx
    1d7e:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1d85 <_dl_relocate_object+0x1a85>	1d81: R_X86_64_PC32	.LC12-0x4
    1d85:	48 0f 44 f0          	cmove  %rax,%rsi
    1d89:	31 c0                	xor    %eax,%eax
    1d8b:	4c 89 95 40 ff ff ff 	mov    %r10,-0xc0(%rbp)
    1d92:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    1d99:	4c 89 8d 78 ff ff ff 	mov    %r9,-0x88(%rbp)
    1da0:	e8 00 00 00 00       	call   1da5 <_dl_relocate_object+0x1aa5>	1da1: R_X86_64_PLT32	_dl_error_printf-0x4
    1da5:	4c 8b 95 40 ff ff ff 	mov    -0xc0(%rbp),%r10
    1dac:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    1db3:	4c 8b 8d 78 ff ff ff 	mov    -0x88(%rbp),%r9
    1dba:	48 8b 85 60 ff ff ff 	mov    -0xa0(%rbp),%rax
    1dc1:	48 8b b5 68 ff ff ff 	mov    -0x98(%rbp),%rsi
    1dc8:	4c 89 95 40 ff ff ff 	mov    %r10,-0xc0(%rbp)
    1dcf:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    1dd6:	4c 89 8d 78 ff ff ff 	mov    %r9,-0x88(%rbp)
    1ddd:	48 01 f0             	add    %rsi,%rax
    1de0:	ff d0                	call   *%rax
    1de2:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    1de6:	4c 8b 8d 78 ff ff ff 	mov    -0x88(%rbp),%r9
    1ded:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    1df4:	4c 8b 95 40 ff ff ff 	mov    -0xc0(%rbp),%r10
    1dfb:	48 85 d2             	test   %rdx,%rdx
    1dfe:	0f 85 e7 f2 ff ff    	jne    10eb <_dl_relocate_object+0xdeb>
    1e04:	e9 f1 ea ff ff       	jmp    8fa <_dl_relocate_object+0x5fa>
    1e09:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    1e10:	49 83 fd 24          	cmp    $0x24,%r13
    1e14:	0f 84 c6 fb ff ff    	je     19e0 <_dl_relocate_object+0x16e0>
    1e1a:	45 31 c9             	xor    %r9d,%r9d
    1e1d:	85 c0                	test   %eax,%eax
    1e1f:	0f 85 80 f5 ff ff    	jne    13a5 <_dl_relocate_object+0x10a5>
    1e25:	e9 74 fa ff ff       	jmp    189e <_dl_relocate_object+0x159e>
    1e2a:	48 8d 75 88          	lea    -0x78(%rbp),%rsi
    1e2e:	48 89 85 10 ff ff ff 	mov    %rax,-0xf0(%rbp)
    1e35:	4d 89 eb             	mov    %r13,%r11
    1e38:	48 c7 85 18 ff ff ff 00 00 00 00 	movq   $0x0,-0xe8(%rbp)
    1e43:	48 89 b5 40 ff ff ff 	mov    %rsi,-0xc0(%rbp)
    1e4a:	66 0f 1f 44 00 00    	nopw   0x0(%rax,%rax,1)
    1e50:	4c 8b 7b 08          	mov    0x8(%rbx),%r15
    1e54:	48 8b bd 68 ff ff ff 	mov    -0x98(%rbp),%rdi
    1e5b:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
    1e62:	4c 8b 13             	mov    (%rbx),%r10
    1e65:	4c 89 f8             	mov    %r15,%rax
    1e68:	49 8b 8c 24 20 03 00 00 	mov    0x320(%r12),%rcx
    1e70:	45 89 fe             	mov    %r15d,%r14d
    1e73:	48 c1 e8 20          	shr    $0x20,%rax
    1e77:	4d 01 da             	add    %r11,%r10
    1e7a:	0f b7 14 47          	movzwl (%rdi,%rax,2),%edx
    1e7e:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    1e82:	4c 8d 2c c6          	lea    (%rsi,%rax,8),%r13
    1e86:	41 83 ff 25          	cmp    $0x25,%r15d
    1e8a:	0f 84 d0 04 00 00    	je     2360 <_dl_relocate_object+0x2060>
    1e90:	4c 89 6d 88          	mov    %r13,-0x78(%rbp)
    1e94:	49 83 fe 08          	cmp    $0x8,%r14
    1e98:	0f 84 52 01 00 00    	je     1ff0 <_dl_relocate_object+0x1cf0>
    1e9e:	49 83 fe 26          	cmp    $0x26,%r14
    1ea2:	0f 84 48 01 00 00    	je     1ff0 <_dl_relocate_object+0x1cf0>
    1ea8:	4d 85 f6             	test   %r14,%r14
    1eab:	0f 84 4f 01 00 00    	je     2000 <_dl_relocate_object+0x1d00>
    1eb1:	41 0f b6 45 04       	movzbl 0x4(%r13),%eax
    1eb6:	c0 e8 04             	shr    $0x4,%al
    1eb9:	0f 84 91 04 00 00    	je     2350 <_dl_relocate_object+0x2050>
    1ebf:	41 0f b6 45 05       	movzbl 0x5(%r13),%eax
    1ec4:	83 e0 03             	and    $0x3,%eax
    1ec7:	83 e8 01             	sub    $0x1,%eax
    1eca:	83 f8 01             	cmp    $0x1,%eax
    1ecd:	0f 86 7d 04 00 00    	jbe    2350 <_dl_relocate_object+0x2050>
    1ed3:	4d 39 ac 24 40 04 00 00 	cmp    %r13,0x440(%r12)
    1edb:	0f 84 fa 04 00 00    	je     23db <_dl_relocate_object+0x20db>
    1ee1:	49 83 fe 12          	cmp    $0x12,%r14
    1ee5:	0f 87 a6 04 00 00    	ja     2391 <_dl_relocate_object+0x2091>
    1eeb:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    1ef1:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    1ef8:	75 17                	jne    1f11 <_dl_relocate_object+0x1c11>
    1efa:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    1f00:	49 83 fe 05          	cmp    $0x5,%r14
    1f04:	74 0b                	je     1f11 <_dl_relocate_object+0x1c11>
    1f06:	45 31 c9             	xor    %r9d,%r9d
    1f09:	49 83 fe 07          	cmp    $0x7,%r14
    1f0d:	41 0f 94 c1          	sete   %r9b
    1f11:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    1f16:	41 8b 75 00          	mov    0x0(%r13),%esi
    1f1a:	45 89 8c 24 48 04 00 00 	mov    %r9d,0x448(%r12)
    1f22:	4d 89 ac 24 40 04 00 00 	mov    %r13,0x440(%r12)
    1f2a:	48 8b 78 08          	mov    0x8(%rax),%rdi
    1f2e:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    1f37:	0f 85 4b 04 00 00    	jne    2388 <_dl_relocate_object+0x2088>
    1f3d:	31 c0                	xor    %eax,%eax
    1f3f:	81 e2 ff 7f 00 00    	and    $0x7fff,%edx
    1f45:	48 01 f7             	add    %rsi,%rdi
    1f48:	48 8d 14 52          	lea    (%rdx,%rdx,2),%rdx
    1f4c:	48 01 c7             	add    %rax,%rdi
    1f4f:	4c 8d 04 d1          	lea    (%rcx,%rdx,8),%r8
    1f53:	4d 85 c0             	test   %r8,%r8
    1f56:	74 0b                	je     1f63 <_dl_relocate_object+0x1c63>
    1f58:	41 8b 40 08          	mov    0x8(%r8),%eax
    1f5c:	85 c0                	test   %eax,%eax
    1f5e:	75 03                	jne    1f63 <_dl_relocate_object+0x1c63>
    1f60:	45 31 c0             	xor    %r8d,%r8d
    1f63:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    1f6a:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    1f71:	4c 89 e6             	mov    %r12,%rsi
    1f74:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    1f7b:	48 8b 95 40 ff ff ff 	mov    -0xc0(%rbp),%rdx
    1f82:	6a 00                	push   $0x0
    1f84:	6a 09                	push   $0x9
    1f86:	e8 00 00 00 00       	call   1f8b <_dl_relocate_object+0x1c8b>	1f87: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    1f8b:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    1f92:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    1f99:	48 89 c2             	mov    %rax,%rdx
    1f9c:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    1fa0:	66 48 0f 6e c2       	movq   %rdx,%xmm0
    1fa5:	66 48 0f 6e f0       	movq   %rax,%xmm6
    1faa:	66 0f 6c c6          	punpcklqdq %xmm6,%xmm0
    1fae:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
    1fb7:	59                   	pop    %rcx
    1fb8:	5e                   	pop    %rsi
    1fb9:	31 f6                	xor    %esi,%esi
    1fbb:	48 85 c0             	test   %rax,%rax
    1fbe:	74 12                	je     1fd2 <_dl_relocate_object+0x1cd2>
    1fc0:	66 83 78 06 f1       	cmpw   $0xfff1,0x6(%rax)
    1fc5:	0f 84 df 03 00 00    	je     23aa <_dl_relocate_object+0x20aa>
    1fcb:	48 8b 32             	mov    (%rdx),%rsi
    1fce:	48 03 70 08          	add    0x8(%rax),%rsi
    1fd2:	49 83 fe 25          	cmp    $0x25,%r14
    1fd6:	0f 87 b4 eb ff ff    	ja     b90 <_dl_relocate_object+0x890>
    1fdc:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 1fe3 <_dl_relocate_object+0x1ce3>	1fdf: R_X86_64_PC32	.rodata+0x12c
    1fe3:	4a 63 04 b7          	movslq (%rdi,%r14,4),%rax
    1fe7:	48 01 f8             	add    %rdi,%rax
    1fea:	3e ff e0             	notrack jmp *%rax
    1fed:	0f 1f 00             	nopl   (%rax)
    1ff0:	49 8b 04 24          	mov    (%r12),%rax
    1ff4:	48 03 43 10          	add    0x10(%rbx),%rax
    1ff8:	49 89 02             	mov    %rax,(%r10)
    1ffb:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    2000:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
    2007:	48 83 c3 18          	add    $0x18,%rbx
    200b:	48 39 c3             	cmp    %rax,%rbx
    200e:	0f 82 3c fe ff ff    	jb     1e50 <_dl_relocate_object+0x1b50>
    2014:	4d 89 dd             	mov    %r11,%r13
    2017:	4c 8b 9d 38 ff ff ff 	mov    -0xc8(%rbp),%r11
    201e:	48 8b 85 10 ff ff ff 	mov    -0xf0(%rbp),%rax
    2025:	4d 85 db             	test   %r11,%r11
    2028:	0f 84 8b e6 ff ff    	je     6b9 <_dl_relocate_object+0x3b9>
    202e:	4c 39 9d 18 ff ff ff 	cmp    %r11,-0xe8(%rbp)
    2035:	0f 82 7e e6 ff ff    	jb     6b9 <_dl_relocate_object+0x3b9>
    203b:	48 89 85 70 ff ff ff 	mov    %rax,-0x90(%rbp)
    2042:	4c 89 e0             	mov    %r12,%rax
    2045:	4c 8b b5 18 ff ff ff 	mov    -0xe8(%rbp),%r14
    204c:	4d 89 ec             	mov    %r13,%r12
    204f:	4c 89 db             	mov    %r11,%rbx
    2052:	49 89 c5             	mov    %rax,%r13
    2055:	eb 16                	jmp    206d <_dl_relocate_object+0x1d6d>
    2057:	66 0f 1f 84 00 00 00 00 00 	nopw   0x0(%rax,%rax,1)
    2060:	48 83 c3 18          	add    $0x18,%rbx
    2064:	49 39 de             	cmp    %rbx,%r14
    2067:	0f 82 d9 04 00 00    	jb     2546 <_dl_relocate_object+0x2246>
    206d:	48 8b 43 08          	mov    0x8(%rbx),%rax
    2071:	83 f8 25             	cmp    $0x25,%eax
    2074:	75 ea                	jne    2060 <_dl_relocate_object+0x1d60>
    2076:	48 8b bd 68 ff ff ff 	mov    -0x98(%rbp),%rdi
    207d:	48 c1 e8 20          	shr    $0x20,%rax
    2081:	4c 8b 3b             	mov    (%rbx),%r15
    2084:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
    208b:	4d 8b 85 20 03 00 00 	mov    0x320(%r13),%r8
    2092:	0f b7 3c 47          	movzwl (%rdi,%rax,2),%edi
    2096:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    209a:	4d 01 e7             	add    %r12,%r15
    209d:	48 8d 04 c6          	lea    (%rsi,%rax,8),%rax
    20a1:	48 89 45 88          	mov    %rax,-0x78(%rbp)
    20a5:	0f b6 70 04          	movzbl 0x4(%rax),%esi
    20a9:	40 c0 ee 04          	shr    $0x4,%sil
    20ad:	0f 84 aa 00 00 00    	je     215d <_dl_relocate_object+0x1e5d>
    20b3:	0f b6 48 05          	movzbl 0x5(%rax),%ecx
    20b7:	83 e1 03             	and    $0x3,%ecx
    20ba:	83 e9 01             	sub    $0x1,%ecx
    20bd:	83 f9 01             	cmp    $0x1,%ecx
    20c0:	0f 86 97 00 00 00    	jbe    215d <_dl_relocate_object+0x1e5d>
    20c6:	49 39 85 40 04 00 00 	cmp    %rax,0x440(%r13)
    20cd:	0f 84 98 0a 00 00    	je     2b6b <_dl_relocate_object+0x286b>
    20d3:	49 89 85 40 04 00 00 	mov    %rax,0x440(%r13)
    20da:	8b 10                	mov    (%rax),%edx
    20dc:	41 c7 85 48 04 00 00 00 00 00 00 	movl   $0x0,0x448(%r13)
    20e7:	49 8b 45 68          	mov    0x68(%r13),%rax
    20eb:	48 8b 40 08          	mov    0x8(%rax),%rax
    20ef:	41 f6 85 56 03 00 00 20 	testb  $0x20,0x356(%r13)
    20f7:	0f 85 a4 02 00 00    	jne    23a1 <_dl_relocate_object+0x20a1>
    20fd:	31 c9                	xor    %ecx,%ecx
    20ff:	81 e7 ff 7f 00 00    	and    $0x7fff,%edi
    2105:	48 01 d0             	add    %rdx,%rax
    2108:	48 8d 3c 7f          	lea    (%rdi,%rdi,2),%rdi
    210c:	4d 8d 04 f8          	lea    (%r8,%rdi,8),%r8
    2110:	48 8d 3c 08          	lea    (%rax,%rcx,1),%rdi
    2114:	4d 85 c0             	test   %r8,%r8
    2117:	74 0b                	je     2124 <_dl_relocate_object+0x1e24>
    2119:	41 8b 48 08          	mov    0x8(%r8),%ecx
    211d:	85 c9                	test   %ecx,%ecx
    211f:	75 03                	jne    2124 <_dl_relocate_object+0x1e24>
    2121:	45 31 c0             	xor    %r8d,%r8d
    2124:	6a 00                	push   $0x0
    2126:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    212d:	48 8d 55 88          	lea    -0x78(%rbp),%rdx
    2131:	4c 89 ee             	mov    %r13,%rsi
    2134:	6a 09                	push   $0x9
    2136:	45 31 c9             	xor    %r9d,%r9d
    2139:	e8 00 00 00 00       	call   213e <_dl_relocate_object+0x1e3e>	213a: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    213e:	48 89 c2             	mov    %rax,%rdx
    2141:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2145:	66 48 0f 6e c2       	movq   %rdx,%xmm0
    214a:	66 48 0f 6e e8       	movq   %rax,%xmm5
    214f:	66 0f 6c c5          	punpcklqdq %xmm5,%xmm0
    2153:	41 0f 11 85 50 04 00 00 	movups %xmm0,0x450(%r13)
    215b:	5e                   	pop    %rsi
    215c:	5f                   	pop    %rdi
    215d:	49 8b 45 00          	mov    0x0(%r13),%rax
    2161:	48 03 43 10          	add    0x10(%rbx),%rax
    2165:	49 89 07             	mov    %rax,(%r15)
    2168:	e9 f3 fe ff ff       	jmp    2060 <_dl_relocate_object+0x1d60>
    216d:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2171:	48 8b 70 10          	mov    0x10(%rax),%rsi
    2175:	48 03 73 10          	add    0x10(%rbx),%rsi
    2179:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
    217e:	41 89 32             	mov    %esi,(%r10)
    2181:	48 39 f0             	cmp    %rsi,%rax
    2184:	0f 83 76 fe ff ff    	jae    2000 <_dl_relocate_object+0x1d00>
    218a:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2191 <_dl_relocate_object+0x1e91>	218d: R_X86_64_PC32	.LC8-0x4
    2191:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    2196:	48 8b 40 08          	mov    0x8(%rax),%rax
    219a:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    21a3:	74 04                	je     21a9 <_dl_relocate_object+0x1ea9>
    21a5:	49 03 04 24          	add    (%r12),%rax
    21a9:	41 8b 55 00          	mov    0x0(%r13),%edx
    21ad:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    21b4:	48 01 c2             	add    %rax,%rdx
    21b7:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 21be <_dl_relocate_object+0x1ebe>	21ba: R_X86_64_PC32	_dl_argv-0x4
    21be:	48 8b 30             	mov    (%rax),%rsi
    21c1:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 21c8 <_dl_relocate_object+0x1ec8>	21c4: R_X86_64_PC32	.LC6-0x4
    21c8:	48 85 f6             	test   %rsi,%rsi
    21cb:	48 0f 44 f0          	cmove  %rax,%rsi
    21cf:	31 c0                	xor    %eax,%eax
    21d1:	e8 00 00 00 00       	call   21d6 <_dl_relocate_object+0x1ed6>	21d2: R_X86_64_PLT32	_dl_error_printf-0x4
    21d6:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    21dd:	e9 1e fe ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    21e2:	41 c6 84 24 59 03 00 00 01 	movb   $0x1,0x359(%r12)
    21eb:	49 89 32             	mov    %rsi,(%r10)
    21ee:	e9 0d fe ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    21f3:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    21f7:	48 85 c0             	test   %rax,%rax
    21fa:	0f 84 d2 08 00 00    	je     2ad2 <_dl_relocate_object+0x27d2>
    2200:	48 8b 8a 88 04 00 00 	mov    0x488(%rdx),%rcx
    2207:	48 8d 71 01          	lea    0x1(%rcx),%rsi
    220b:	48 83 fe 01          	cmp    $0x1,%rsi
    220f:	0f 86 dc 09 00 00    	jbe    2bf1 <_dl_relocate_object+0x28f1>
    2215:	48 8b 40 08          	mov    0x8(%rax),%rax
    2219:	48 29 c8             	sub    %rcx,%rax
    221c:	48 03 43 10          	add    0x10(%rbx),%rax
    2220:	49 89 42 08          	mov    %rax,0x8(%r10)
    2224:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 222b <_dl_relocate_object+0x1f2b>	2227: R_X86_64_PC32	_dl_tlsdesc_return-0x4
    222b:	49 89 02             	mov    %rax,(%r10)
    222e:	e9 cd fd ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2233:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    2237:	48 8b 43 10          	mov    0x10(%rbx),%rax
    223b:	48 03 42 10          	add    0x10(%rdx),%rax
    223f:	49 89 02             	mov    %rax,(%r10)
    2242:	e9 b9 fd ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2247:	4c 8b 75 88          	mov    -0x78(%rbp),%r14
    224b:	4d 85 f6             	test   %r14,%r14
    224e:	0f 84 ac fd ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    2254:	49 8b 46 10          	mov    0x10(%r14),%rax
    2258:	49 8b 55 10          	mov    0x10(%r13),%rdx
    225c:	4c 89 d7             	mov    %r10,%rdi
    225f:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    2266:	48 39 d0             	cmp    %rdx,%rax
    2269:	48 0f 46 d0          	cmovbe %rax,%rdx
    226d:	e8 00 00 00 00       	call   2272 <_dl_relocate_object+0x1f72>	226e: R_X86_64_PLT32	memcpy-0x4
    2272:	49 8b 56 10          	mov    0x10(%r14),%rdx
    2276:	49 8b 45 10          	mov    0x10(%r13),%rax
    227a:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    2281:	48 39 d0             	cmp    %rdx,%rax
    2284:	72 17                	jb     229d <_dl_relocate_object+0x1f9d>
    2286:	48 39 c2             	cmp    %rax,%rdx
    2289:	0f 83 71 fd ff ff    	jae    2000 <_dl_relocate_object+0x1d00>
    228f:	8b 3d 00 00 00 00    	mov    0x0(%rip),%edi        # 2295 <_dl_relocate_object+0x1f95>	2291: R_X86_64_PC32	_dl_verbose-0x4
    2295:	85 ff                	test   %edi,%edi
    2297:	0f 84 63 fd ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    229d:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 22a4 <_dl_relocate_object+0x1fa4>	22a0: R_X86_64_PC32	.LC7-0x4
    22a4:	e9 e8 fe ff ff       	jmp    2191 <_dl_relocate_object+0x1e91>
    22a9:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    22b0:	48 03 73 10          	add    0x10(%rbx),%rsi
    22b4:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 22bb <_dl_relocate_object+0x1fbb>	22b7: R_X86_64_PC32	.LC9-0x4
    22bb:	4c 29 d6             	sub    %r10,%rsi
    22be:	48 63 c6             	movslq %esi,%rax
    22c1:	41 89 32             	mov    %esi,(%r10)
    22c4:	48 39 f0             	cmp    %rsi,%rax
    22c7:	0f 84 33 fd ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    22cd:	e9 bf fe ff ff       	jmp    2191 <_dl_relocate_object+0x1e91>
    22d2:	48 03 73 10          	add    0x10(%rbx),%rsi
    22d6:	49 89 32             	mov    %rsi,(%r10)
    22d9:	e9 22 fd ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    22de:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    22e2:	48 85 c0             	test   %rax,%rax
    22e5:	0f 84 15 fd ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    22eb:	48 8b 8a 88 04 00 00 	mov    0x488(%rdx),%rcx
    22f2:	48 8d 71 01          	lea    0x1(%rcx),%rsi
    22f6:	48 83 fe 01          	cmp    $0x1,%rsi
    22fa:	0f 86 75 09 00 00    	jbe    2c75 <_dl_relocate_object+0x2975>
    2300:	48 8b 40 08          	mov    0x8(%rax),%rax
    2304:	48 29 c8             	sub    %rcx,%rax
    2307:	48 03 43 10          	add    0x10(%rbx),%rax
    230b:	49 89 02             	mov    %rax,(%r10)
    230e:	e9 ed fc ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2313:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2317:	48 85 c0             	test   %rax,%rax
    231a:	0f 84 e0 fc ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    2320:	48 8b 40 08          	mov    0x8(%rax),%rax
    2324:	48 03 43 10          	add    0x10(%rbx),%rax
    2328:	49 89 02             	mov    %rax,(%r10)
    232b:	e9 d0 fc ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2330:	48 85 d2             	test   %rdx,%rdx
    2333:	0f 84 c7 fc ff ff    	je     2000 <_dl_relocate_object+0x1d00>
    2339:	48 8b 82 90 04 00 00 	mov    0x490(%rdx),%rax
    2340:	49 89 02             	mov    %rax,(%r10)
    2343:	e9 b8 fc ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2348:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
    2350:	4c 89 e2             	mov    %r12,%rdx
    2353:	4c 89 e8             	mov    %r13,%rax
    2356:	e9 65 fc ff ff       	jmp    1fc0 <_dl_relocate_object+0x1cc0>
    235b:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    2360:	48 83 bd 38 ff ff ff 00 	cmpq   $0x0,-0xc8(%rbp)
    2368:	48 89 9d 18 ff ff ff 	mov    %rbx,-0xe8(%rbp)
    236f:	0f 85 8b fc ff ff    	jne    2000 <_dl_relocate_object+0x1d00>
    2375:	48 89 9d 38 ff ff ff 	mov    %rbx,-0xc8(%rbp)
    237c:	e9 7f fc ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2381:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    2388:	49 8b 04 24          	mov    (%r12),%rax
    238c:	e9 ae fb ff ff       	jmp    1f3f <_dl_relocate_object+0x1c3f>
    2391:	45 31 c9             	xor    %r9d,%r9d
    2394:	49 83 fe 24          	cmp    $0x24,%r14
    2398:	41 0f 94 c1          	sete   %r9b
    239c:	e9 70 fb ff ff       	jmp    1f11 <_dl_relocate_object+0x1c11>
    23a1:	49 8b 4d 00          	mov    0x0(%r13),%rcx
    23a5:	e9 55 fd ff ff       	jmp    20ff <_dl_relocate_object+0x1dff>
    23aa:	31 f6                	xor    %esi,%esi
    23ac:	e9 1d fc ff ff       	jmp    1fce <_dl_relocate_object+0x1cce>
    23b1:	45 8b 84 24 48 04 00 00 	mov    0x448(%r12),%r8d
    23b9:	45 85 c0             	test   %r8d,%r8d
    23bc:	0f 85 7c f1 ff ff    	jne    153e <_dl_relocate_object+0x123e>
    23c2:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    23ca:	49 8b 8c 24 50 04 00 00 	mov    0x450(%r12),%rcx
    23d2:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    23d6:	e9 e5 f1 ff ff       	jmp    15c0 <_dl_relocate_object+0x12c0>
    23db:	41 8b 84 24 48 04 00 00 	mov    0x448(%r12),%eax
    23e3:	49 83 fe 12          	cmp    $0x12,%r14
    23e7:	0f 87 64 07 00 00    	ja     2b51 <_dl_relocate_object+0x2851>
    23ed:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    23f4:	75 14                	jne    240a <_dl_relocate_object+0x210a>
    23f6:	49 83 fe 05          	cmp    $0x5,%r14
    23fa:	0f 84 7f 07 00 00    	je     2b7f <_dl_relocate_object+0x287f>
    2400:	49 83 fe 07          	cmp    $0x7,%r14
    2404:	0f 85 a0 07 00 00    	jne    2baa <_dl_relocate_object+0x28aa>
    240a:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    2410:	83 f8 01             	cmp    $0x1,%eax
    2413:	0f 85 f8 fa ff ff    	jne    1f11 <_dl_relocate_object+0x1c11>
    2419:	49 8b 84 24 58 04 00 00 	mov    0x458(%r12),%rax
    2421:	49 8b 94 24 50 04 00 00 	mov    0x450(%r12),%rdx
    2429:	48 89 45 88          	mov    %rax,-0x78(%rbp)
    242d:	e9 87 fb ff ff       	jmp    1fb9 <_dl_relocate_object+0x1cb9>
    2432:	49 39 cc             	cmp    %rcx,%r12
    2435:	0f 84 80 00 00 00    	je     24bb <_dl_relocate_object+0x21bb>
    243b:	0f b6 b9 54 03 00 00 	movzbl 0x354(%rcx),%edi
    2442:	40 f6 c7 08          	test   $0x8,%dil
    2446:	75 73                	jne    24bb <_dl_relocate_object+0x21bb>
    2448:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    244d:	48 8b 40 08          	mov    0x8(%rax),%rax
    2451:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    245a:	74 04                	je     2460 <_dl_relocate_object+0x2160>
    245c:	49 03 04 24          	add    (%r12),%rax
    2460:	41 8b 12             	mov    (%r10),%edx
    2463:	83 e7 03             	and    $0x3,%edi
    2466:	4c 8d 04 10          	lea    (%rax,%rdx,1),%r8
    246a:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 2471 <_dl_relocate_object+0x2171>	246d: R_X86_64_PC32	_dl_argv-0x4
    2471:	49 8b 54 24 08       	mov    0x8(%r12),%rdx
    2476:	48 8b 30             	mov    (%rax),%rsi
    2479:	0f 84 31 09 00 00    	je     2db0 <_dl_relocate_object+0x2ab0>
    247f:	48 85 f6             	test   %rsi,%rsi
    2482:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 2489 <_dl_relocate_object+0x2189>	2485: R_X86_64_PC32	.LC6-0x4
    2489:	48 8b 49 08          	mov    0x8(%rcx),%rcx
    248d:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2494 <_dl_relocate_object+0x2194>	2490: R_X86_64_PC32	.LC12-0x4
    2494:	48 0f 44 f0          	cmove  %rax,%rsi
    2498:	31 c0                	xor    %eax,%eax
    249a:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    24a1:	4c 89 8d 70 ff ff ff 	mov    %r9,-0x90(%rbp)
    24a8:	e8 00 00 00 00       	call   24ad <_dl_relocate_object+0x21ad>	24a9: R_X86_64_PLT32	_dl_error_printf-0x4
    24ad:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    24b4:	4c 8b 8d 70 ff ff ff 	mov    -0x90(%rbp),%r9
    24bb:	4d 01 cb             	add    %r9,%r11
    24be:	41 ff d3             	call   *%r11
    24c1:	48 8b 43 10          	mov    0x10(%rbx),%rax
    24c5:	49 03 04 24          	add    (%r12),%rax
    24c9:	e9 28 f1 ff ff       	jmp    15f6 <_dl_relocate_object+0x12f6>
    24ce:	45 31 c9             	xor    %r9d,%r9d
    24d1:	85 c0                	test   %eax,%eax
    24d3:	0f 85 cf e5 ff ff    	jne    aa8 <_dl_relocate_object+0x7a8>
    24d9:	e9 a0 eb ff ff       	jmp    107e <_dl_relocate_object+0xd7e>
    24de:	4c 89 df             	mov    %r11,%rdi
    24e1:	4c 89 95 10 ff ff ff 	mov    %r10,-0xf0(%rbp)
    24e8:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    24ef:	e8 00 00 00 00       	call   24f4 <_dl_relocate_object+0x21f4>	24f0: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    24f4:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    24fb:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    24ff:	4c 8b 95 10 ff ff ff 	mov    -0xf0(%rbp),%r10
    2506:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    250d:	e9 00 ea ff ff       	jmp    f12 <_dl_relocate_object+0xc12>
    2512:	4c 89 df             	mov    %r11,%rdi
    2515:	4c 89 95 10 ff ff ff 	mov    %r10,-0xf0(%rbp)
    251c:	4c 89 9d 50 ff ff ff 	mov    %r11,-0xb0(%rbp)
    2523:	e8 00 00 00 00       	call   2528 <_dl_relocate_object+0x2228>	2524: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2528:	4c 8b 9d 50 ff ff ff 	mov    -0xb0(%rbp),%r11
    252f:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2533:	4c 8b 95 10 ff ff ff 	mov    -0xf0(%rbp),%r10
    253a:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    2541:	e9 24 ea ff ff       	jmp    f6a <_dl_relocate_object+0xc6a>
    2546:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
    254d:	4d 89 ec             	mov    %r13,%r12
    2550:	e9 64 e1 ff ff       	jmp    6b9 <_dl_relocate_object+0x3b9>
    2555:	48 8d 7d 88          	lea    -0x78(%rbp),%rdi
    2559:	4c 89 95 40 ff ff ff 	mov    %r10,-0xc0(%rbp)
    2560:	4d 89 eb             	mov    %r13,%r11
    2563:	48 c7 85 38 ff ff ff 00 00 00 00 	movq   $0x0,-0xc8(%rbp)
    256e:	48 89 bd 50 ff ff ff 	mov    %rdi,-0xb0(%rbp)
    2575:	48 89 85 18 ff ff ff 	mov    %rax,-0xe8(%rbp)
    257c:	0f 1f 40 00          	nopl   0x0(%rax)
    2580:	4c 8b 7b 08          	mov    0x8(%rbx),%r15
    2584:	48 8b b5 78 ff ff ff 	mov    -0x88(%rbp),%rsi
    258b:	4c 8b 13             	mov    (%rbx),%r10
    258e:	4c 89 f8             	mov    %r15,%rax
    2591:	45 89 fe             	mov    %r15d,%r14d
    2594:	48 c1 e8 20          	shr    $0x20,%rax
    2598:	4d 01 da             	add    %r11,%r10
    259b:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    259f:	4c 8d 2c c6          	lea    (%rsi,%rax,8),%r13
    25a3:	41 83 ff 25          	cmp    $0x25,%r15d
    25a7:	0f 84 84 04 00 00    	je     2a31 <_dl_relocate_object+0x2731>
    25ad:	4c 89 6d 88          	mov    %r13,-0x78(%rbp)
    25b1:	49 83 fe 08          	cmp    $0x8,%r14
    25b5:	0f 84 32 01 00 00    	je     26ed <_dl_relocate_object+0x23ed>
    25bb:	49 83 fe 26          	cmp    $0x26,%r14
    25bf:	0f 84 28 01 00 00    	je     26ed <_dl_relocate_object+0x23ed>
    25c5:	4d 85 f6             	test   %r14,%r14
    25c8:	0f 84 32 01 00 00    	je     2700 <_dl_relocate_object+0x2400>
    25ce:	41 0f b6 45 04       	movzbl 0x4(%r13),%eax
    25d3:	c0 e8 04             	shr    $0x4,%al
    25d6:	0f 84 4a 04 00 00    	je     2a26 <_dl_relocate_object+0x2726>
    25dc:	41 0f b6 45 05       	movzbl 0x5(%r13),%eax
    25e1:	83 e0 03             	and    $0x3,%eax
    25e4:	83 e8 01             	sub    $0x1,%eax
    25e7:	83 f8 01             	cmp    $0x1,%eax
    25ea:	0f 86 36 04 00 00    	jbe    2a26 <_dl_relocate_object+0x2726>
    25f0:	4d 39 ac 24 40 04 00 00 	cmp    %r13,0x440(%r12)
    25f8:	0f 84 7d 04 00 00    	je     2a7b <_dl_relocate_object+0x277b>
    25fe:	49 83 fe 12          	cmp    $0x12,%r14
    2602:	0f 87 53 04 00 00    	ja     2a5b <_dl_relocate_object+0x275b>
    2608:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    260e:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    2615:	75 17                	jne    262e <_dl_relocate_object+0x232e>
    2617:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    261d:	49 83 fe 05          	cmp    $0x5,%r14
    2621:	74 0b                	je     262e <_dl_relocate_object+0x232e>
    2623:	45 31 c9             	xor    %r9d,%r9d
    2626:	49 83 fe 07          	cmp    $0x7,%r14
    262a:	41 0f 94 c1          	sete   %r9b
    262e:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    2633:	41 8b 55 00          	mov    0x0(%r13),%edx
    2637:	45 89 8c 24 48 04 00 00 	mov    %r9d,0x448(%r12)
    263f:	4d 89 ac 24 40 04 00 00 	mov    %r13,0x440(%r12)
    2647:	48 8b 40 08          	mov    0x8(%rax),%rax
    264b:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    2654:	0f 85 f8 03 00 00    	jne    2a52 <_dl_relocate_object+0x2752>
    265a:	31 c9                	xor    %ecx,%ecx
    265c:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    2663:	48 01 d0             	add    %rdx,%rax
    2666:	4c 89 e6             	mov    %r12,%rsi
    2669:	45 31 c0             	xor    %r8d,%r8d
    266c:	4c 89 95 68 ff ff ff 	mov    %r10,-0x98(%rbp)
    2673:	48 8d 3c 08          	lea    (%rax,%rcx,1),%rdi
    2677:	48 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%rdx
    267e:	6a 00                	push   $0x0
    2680:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    2687:	6a 09                	push   $0x9
    2689:	e8 00 00 00 00       	call   268e <_dl_relocate_object+0x238e>	268a: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    268e:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    2692:	4c 8b 95 68 ff ff ff 	mov    -0x98(%rbp),%r10
    2699:	66 48 0f 6e c0       	movq   %rax,%xmm0
    269e:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    26a5:	66 48 0f 6e fa       	movq   %rdx,%xmm7
    26aa:	66 0f 6c c7          	punpcklqdq %xmm7,%xmm0
    26ae:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
    26b7:	5e                   	pop    %rsi
    26b8:	5f                   	pop    %rdi
    26b9:	31 c9                	xor    %ecx,%ecx
    26bb:	48 85 d2             	test   %rdx,%rdx
    26be:	74 12                	je     26d2 <_dl_relocate_object+0x23d2>
    26c0:	66 83 7a 06 f1       	cmpw   $0xfff1,0x6(%rdx)
    26c5:	0f 84 a9 03 00 00    	je     2a74 <_dl_relocate_object+0x2774>
    26cb:	48 8b 08             	mov    (%rax),%rcx
    26ce:	48 03 4a 08          	add    0x8(%rdx),%rcx
    26d2:	49 83 fe 25          	cmp    $0x25,%r14
    26d6:	0f 87 b4 e4 ff ff    	ja     b90 <_dl_relocate_object+0x890>
    26dc:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 26e3 <_dl_relocate_object+0x23e3>	26df: R_X86_64_PC32	.rodata+0x1c4
    26e3:	4a 63 14 b6          	movslq (%rsi,%r14,4),%rdx
    26e7:	48 01 f2             	add    %rsi,%rdx
    26ea:	3e ff e2             	notrack jmp *%rdx
    26ed:	49 8b 04 24          	mov    (%r12),%rax
    26f1:	48 03 43 10          	add    0x10(%rbx),%rax
    26f5:	49 89 02             	mov    %rax,(%r10)
    26f8:	0f 1f 84 00 00 00 00 00 	nopl   0x0(%rax,%rax,1)
    2700:	48 8b 85 70 ff ff ff 	mov    -0x90(%rbp),%rax
    2707:	48 83 c3 18          	add    $0x18,%rbx
    270b:	48 39 c3             	cmp    %rax,%rbx
    270e:	0f 82 6c fe ff ff    	jb     2580 <_dl_relocate_object+0x2280>
    2714:	4c 8b 95 40 ff ff ff 	mov    -0xc0(%rbp),%r10
    271b:	48 8b 85 18 ff ff ff 	mov    -0xe8(%rbp),%rax
    2722:	4d 89 dd             	mov    %r11,%r13
    2725:	4d 85 d2             	test   %r10,%r10
    2728:	0f 84 8b df ff ff    	je     6b9 <_dl_relocate_object+0x3b9>
    272e:	4c 8d 75 88          	lea    -0x78(%rbp),%r14
    2732:	4c 39 95 38 ff ff ff 	cmp    %r10,-0xc8(%rbp)
    2739:	0f 82 7a df ff ff    	jb     6b9 <_dl_relocate_object+0x3b9>
    273f:	48 89 85 68 ff ff ff 	mov    %rax,-0x98(%rbp)
    2746:	4c 8b bd 38 ff ff ff 	mov    -0xc8(%rbp),%r15
    274d:	4c 89 d3             	mov    %r10,%rbx
    2750:	4c 89 b5 70 ff ff ff 	mov    %r14,-0x90(%rbp)
    2757:	eb 14                	jmp    276d <_dl_relocate_object+0x246d>
    2759:	0f 1f 80 00 00 00 00 	nopl   0x0(%rax)
    2760:	48 83 c3 18          	add    $0x18,%rbx
    2764:	49 39 df             	cmp    %rbx,%r15
    2767:	0f 82 9b ee ff ff    	jb     1608 <_dl_relocate_object+0x1308>
    276d:	48 8b 43 08          	mov    0x8(%rbx),%rax
    2771:	83 f8 25             	cmp    $0x25,%eax
    2774:	75 ea                	jne    2760 <_dl_relocate_object+0x2460>
    2776:	48 8b bd 78 ff ff ff 	mov    -0x88(%rbp),%rdi
    277d:	48 c1 e8 20          	shr    $0x20,%rax
    2781:	4c 8b 33             	mov    (%rbx),%r14
    2784:	48 8d 04 40          	lea    (%rax,%rax,2),%rax
    2788:	48 8d 04 c7          	lea    (%rdi,%rax,8),%rax
    278c:	4d 01 ee             	add    %r13,%r14
    278f:	48 89 45 88          	mov    %rax,-0x78(%rbp)
    2793:	0f b6 78 04          	movzbl 0x4(%rax),%edi
    2797:	40 c0 ef 04          	shr    $0x4,%dil
    279b:	0f 84 9a 00 00 00    	je     283b <_dl_relocate_object+0x253b>
    27a1:	0f b6 48 05          	movzbl 0x5(%rax),%ecx
    27a5:	83 e1 03             	and    $0x3,%ecx
    27a8:	83 e9 01             	sub    $0x1,%ecx
    27ab:	83 f9 01             	cmp    $0x1,%ecx
    27ae:	0f 86 87 00 00 00    	jbe    283b <_dl_relocate_object+0x253b>
    27b4:	49 39 84 24 40 04 00 00 	cmp    %rax,0x440(%r12)
    27bc:	0f 84 f8 03 00 00    	je     2bba <_dl_relocate_object+0x28ba>
    27c2:	49 89 84 24 40 04 00 00 	mov    %rax,0x440(%r12)
    27ca:	8b 10                	mov    (%rax),%edx
    27cc:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    27d1:	41 c7 84 24 48 04 00 00 00 00 00 00 	movl   $0x0,0x448(%r12)
    27dd:	48 8b 40 08          	mov    0x8(%rax),%rax
    27e1:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    27ea:	0f 85 7b 02 00 00    	jne    2a6b <_dl_relocate_object+0x276b>
    27f0:	31 c9                	xor    %ecx,%ecx
    27f2:	6a 00                	push   $0x0
    27f4:	48 01 d0             	add    %rdx,%rax
    27f7:	48 8b 95 70 ff ff ff 	mov    -0x90(%rbp),%rdx
    27fe:	45 31 c9             	xor    %r9d,%r9d
    2801:	6a 09                	push   $0x9
    2803:	48 8d 3c 08          	lea    (%rax,%rcx,1),%rdi
    2807:	48 8b 8d 58 ff ff ff 	mov    -0xa8(%rbp),%rcx
    280e:	45 31 c0             	xor    %r8d,%r8d
    2811:	4c 89 e6             	mov    %r12,%rsi
    2814:	e8 00 00 00 00       	call   2819 <_dl_relocate_object+0x2519>	2815: R_X86_64_PLT32	_dl_lookup_symbol_x-0x4
    2819:	48 89 c2             	mov    %rax,%rdx
    281c:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2820:	66 48 0f 6e c2       	movq   %rdx,%xmm0
    2825:	66 48 0f 6e e0       	movq   %rax,%xmm4
    282a:	66 0f 6c c4          	punpcklqdq %xmm4,%xmm0
    282e:	41 0f 11 84 24 50 04 00 00 	movups %xmm0,0x450(%r12)
    2837:	41 5a                	pop    %r10
    2839:	41 5b                	pop    %r11
    283b:	49 8b 04 24          	mov    (%r12),%rax
    283f:	48 03 43 10          	add    0x10(%rbx),%rax
    2843:	49 89 06             	mov    %rax,(%r14)
    2846:	e9 15 ff ff ff       	jmp    2760 <_dl_relocate_object+0x2460>
    284b:	41 c6 84 24 59 03 00 00 01 	movb   $0x1,0x359(%r12)
    2854:	49 89 0a             	mov    %rcx,(%r10)
    2857:	e9 a4 fe ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    285c:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2860:	48 8b 48 10          	mov    0x10(%rax),%rcx
    2864:	48 03 4b 10          	add    0x10(%rbx),%rcx
    2868:	b8 ff ff ff ff       	mov    $0xffffffff,%eax
    286d:	41 89 0a             	mov    %ecx,(%r10)
    2870:	48 39 c8             	cmp    %rcx,%rax
    2873:	0f 83 87 fe ff ff    	jae    2700 <_dl_relocate_object+0x2400>
    2879:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2880 <_dl_relocate_object+0x2580>	287c: R_X86_64_PC32	.LC8-0x4
    2880:	49 8b 44 24 68       	mov    0x68(%r12),%rax
    2885:	48 8b 40 08          	mov    0x8(%rax),%rax
    2889:	41 f6 84 24 56 03 00 00 20 	testb  $0x20,0x356(%r12)
    2892:	74 04                	je     2898 <_dl_relocate_object+0x2598>
    2894:	49 03 04 24          	add    (%r12),%rax
    2898:	41 8b 55 00          	mov    0x0(%r13),%edx
    289c:	4c 89 9d 68 ff ff ff 	mov    %r11,-0x98(%rbp)
    28a3:	48 01 c2             	add    %rax,%rdx
    28a6:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 28ad <_dl_relocate_object+0x25ad>	28a9: R_X86_64_PC32	_dl_argv-0x4
    28ad:	48 8b 30             	mov    (%rax),%rsi
    28b0:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 28b7 <_dl_relocate_object+0x25b7>	28b3: R_X86_64_PC32	.LC6-0x4
    28b7:	48 85 f6             	test   %rsi,%rsi
    28ba:	48 0f 44 f0          	cmove  %rax,%rsi
    28be:	31 c0                	xor    %eax,%eax
    28c0:	e8 00 00 00 00       	call   28c5 <_dl_relocate_object+0x25c5>	28c1: R_X86_64_PLT32	_dl_error_printf-0x4
    28c5:	4c 8b 9d 68 ff ff ff 	mov    -0x98(%rbp),%r11
    28cc:	e9 2f fe ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    28d1:	48 03 4b 10          	add    0x10(%rbx),%rcx
    28d5:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 28dc <_dl_relocate_object+0x25dc>	28d8: R_X86_64_PC32	.LC9-0x4
    28dc:	48 89 c8             	mov    %rcx,%rax
    28df:	4c 29 d0             	sub    %r10,%rax
    28e2:	48 63 d0             	movslq %eax,%rdx
    28e5:	41 89 02             	mov    %eax,(%r10)
    28e8:	48 39 c2             	cmp    %rax,%rdx
    28eb:	0f 84 0f fe ff ff    	je     2700 <_dl_relocate_object+0x2400>
    28f1:	eb 8d                	jmp    2880 <_dl_relocate_object+0x2580>
    28f3:	48 03 4b 10          	add    0x10(%rbx),%rcx
    28f7:	49 89 0a             	mov    %rcx,(%r10)
    28fa:	e9 01 fe ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    28ff:	4c 8b 75 88          	mov    -0x78(%rbp),%r14
    2903:	4d 85 f6             	test   %r14,%r14
    2906:	0f 84 f4 fd ff ff    	je     2700 <_dl_relocate_object+0x2400>
    290c:	49 8b 46 10          	mov    0x10(%r14),%rax
    2910:	49 8b 55 10          	mov    0x10(%r13),%rdx
    2914:	48 89 ce             	mov    %rcx,%rsi
    2917:	4c 89 d7             	mov    %r10,%rdi
    291a:	4c 89 9d 68 ff ff ff 	mov    %r11,-0x98(%rbp)
    2921:	48 39 d0             	cmp    %rdx,%rax
    2924:	48 0f 46 d0          	cmovbe %rax,%rdx
    2928:	e8 00 00 00 00       	call   292d <_dl_relocate_object+0x262d>	2929: R_X86_64_PLT32	memcpy-0x4
    292d:	49 8b 56 10          	mov    0x10(%r14),%rdx
    2931:	49 8b 45 10          	mov    0x10(%r13),%rax
    2935:	4c 8b 9d 68 ff ff ff 	mov    -0x98(%rbp),%r11
    293c:	48 39 d0             	cmp    %rdx,%rax
    293f:	72 16                	jb     2957 <_dl_relocate_object+0x2657>
    2941:	48 39 c2             	cmp    %rax,%rdx
    2944:	0f 83 b6 fd ff ff    	jae    2700 <_dl_relocate_object+0x2400>
    294a:	83 3d 00 00 00 00 00 	cmpl   $0x0,0x0(%rip)        # 2951 <_dl_relocate_object+0x2651>	294c: R_X86_64_PC32	_dl_verbose-0x5
    2951:	0f 84 a9 fd ff ff    	je     2700 <_dl_relocate_object+0x2400>
    2957:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 295e <_dl_relocate_object+0x265e>	295a: R_X86_64_PC32	.LC7-0x4
    295e:	e9 1d ff ff ff       	jmp    2880 <_dl_relocate_object+0x2580>
    2963:	0f 1f 44 00 00       	nopl   0x0(%rax,%rax,1)
    2968:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    296c:	48 85 c0             	test   %rax,%rax
    296f:	0f 84 8b fd ff ff    	je     2700 <_dl_relocate_object+0x2400>
    2975:	48 8b 40 08          	mov    0x8(%rax),%rax
    2979:	48 03 43 10          	add    0x10(%rbx),%rax
    297d:	49 89 02             	mov    %rax,(%r10)
    2980:	e9 7b fd ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    2985:	48 85 c0             	test   %rax,%rax
    2988:	0f 84 72 fd ff ff    	je     2700 <_dl_relocate_object+0x2400>
    298e:	48 8b 80 90 04 00 00 	mov    0x490(%rax),%rax
    2995:	49 89 02             	mov    %rax,(%r10)
    2998:	e9 63 fd ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    299d:	48 8b 4d 88          	mov    -0x78(%rbp),%rcx
    29a1:	48 85 c9             	test   %rcx,%rcx
    29a4:	0f 84 e9 01 00 00    	je     2b93 <_dl_relocate_object+0x2893>
    29aa:	48 8b 90 88 04 00 00 	mov    0x488(%rax),%rdx
    29b1:	48 8d 72 01          	lea    0x1(%rdx),%rsi
    29b5:	48 83 fe 01          	cmp    $0x1,%rsi
    29b9:	0f 86 08 03 00 00    	jbe    2cc7 <_dl_relocate_object+0x29c7>
    29bf:	48 8b 41 08          	mov    0x8(%rcx),%rax
    29c3:	48 29 d0             	sub    %rdx,%rax
    29c6:	48 03 43 10          	add    0x10(%rbx),%rax
    29ca:	49 89 42 08          	mov    %rax,0x8(%r10)
    29ce:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 29d5 <_dl_relocate_object+0x26d5>	29d1: R_X86_64_PC32	_dl_tlsdesc_return-0x4
    29d5:	49 89 02             	mov    %rax,(%r10)
    29d8:	e9 23 fd ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    29dd:	48 8b 55 88          	mov    -0x78(%rbp),%rdx
    29e1:	48 8b 43 10          	mov    0x10(%rbx),%rax
    29e5:	48 03 42 10          	add    0x10(%rdx),%rax
    29e9:	49 89 02             	mov    %rax,(%r10)
    29ec:	e9 0f fd ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    29f1:	48 8b 4d 88          	mov    -0x78(%rbp),%rcx
    29f5:	48 85 c9             	test   %rcx,%rcx
    29f8:	0f 84 02 fd ff ff    	je     2700 <_dl_relocate_object+0x2400>
    29fe:	48 8b 90 88 04 00 00 	mov    0x488(%rax),%rdx
    2a05:	48 8d 72 01          	lea    0x1(%rdx),%rsi
    2a09:	48 83 fe 01          	cmp    $0x1,%rsi
    2a0d:	0f 86 f6 02 00 00    	jbe    2d09 <_dl_relocate_object+0x2a09>
    2a13:	48 8b 41 08          	mov    0x8(%rcx),%rax
    2a17:	48 29 d0             	sub    %rdx,%rax
    2a1a:	48 03 43 10          	add    0x10(%rbx),%rax
    2a1e:	49 89 02             	mov    %rax,(%r10)
    2a21:	e9 da fc ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    2a26:	4c 89 e0             	mov    %r12,%rax
    2a29:	4c 89 ea             	mov    %r13,%rdx
    2a2c:	e9 8f fc ff ff       	jmp    26c0 <_dl_relocate_object+0x23c0>
    2a31:	48 83 bd 40 ff ff ff 00 	cmpq   $0x0,-0xc0(%rbp)
    2a39:	48 89 9d 38 ff ff ff 	mov    %rbx,-0xc8(%rbp)
    2a40:	0f 85 ba fc ff ff    	jne    2700 <_dl_relocate_object+0x2400>
    2a46:	48 89 9d 40 ff ff ff 	mov    %rbx,-0xc0(%rbp)
    2a4d:	e9 ae fc ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    2a52:	49 8b 0c 24          	mov    (%r12),%rcx
    2a56:	e9 01 fc ff ff       	jmp    265c <_dl_relocate_object+0x235c>
    2a5b:	45 31 c9             	xor    %r9d,%r9d
    2a5e:	49 83 fe 24          	cmp    $0x24,%r14
    2a62:	41 0f 94 c1          	sete   %r9b
    2a66:	e9 c3 fb ff ff       	jmp    262e <_dl_relocate_object+0x232e>
    2a6b:	49 8b 0c 24          	mov    (%r12),%rcx
    2a6f:	e9 7e fd ff ff       	jmp    27f2 <_dl_relocate_object+0x24f2>
    2a74:	31 c9                	xor    %ecx,%ecx
    2a76:	e9 53 fc ff ff       	jmp    26ce <_dl_relocate_object+0x23ce>
    2a7b:	41 8b 84 24 48 04 00 00 	mov    0x448(%r12),%eax
    2a83:	49 83 fe 12          	cmp    $0x12,%r14
    2a87:	0f 87 43 01 00 00    	ja     2bd0 <_dl_relocate_object+0x28d0>
    2a8d:	41 f7 c7 f0 ff ff ff 	test   $0xfffffff0,%r15d
    2a94:	75 14                	jne    2aaa <_dl_relocate_object+0x27aa>
    2a96:	49 83 fe 05          	cmp    $0x5,%r14
    2a9a:	0f 84 c1 01 00 00    	je     2c61 <_dl_relocate_object+0x2961>
    2aa0:	49 83 fe 07          	cmp    $0x7,%r14
    2aa4:	0f 85 0d 02 00 00    	jne    2cb7 <_dl_relocate_object+0x29b7>
    2aaa:	41 b9 01 00 00 00    	mov    $0x1,%r9d
    2ab0:	83 f8 01             	cmp    $0x1,%eax
    2ab3:	0f 85 75 fb ff ff    	jne    262e <_dl_relocate_object+0x232e>
    2ab9:	49 8b 94 24 58 04 00 00 	mov    0x458(%r12),%rdx
    2ac1:	49 8b 84 24 50 04 00 00 	mov    0x450(%r12),%rax
    2ac9:	48 89 55 88          	mov    %rdx,-0x78(%rbp)
    2acd:	e9 e7 fb ff ff       	jmp    26b9 <_dl_relocate_object+0x23b9>
    2ad2:	48 8b 43 10          	mov    0x10(%rbx),%rax
    2ad6:	49 89 42 08          	mov    %rax,0x8(%r10)
    2ada:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 2ae1 <_dl_relocate_object+0x27e1>	2add: R_X86_64_PC32	_dl_tlsdesc_undefweak-0x4
    2ae1:	49 89 02             	mov    %rax,(%r10)
    2ae4:	e9 17 f5 ff ff       	jmp    2000 <_dl_relocate_object+0x1d00>
    2ae9:	4c 89 df             	mov    %r11,%rdi
    2aec:	4c 89 95 18 ff ff ff 	mov    %r10,-0xe8(%rbp)
    2af3:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    2afa:	e8 00 00 00 00       	call   2aff <_dl_relocate_object+0x27ff>	2afb: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2aff:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    2b06:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2b0a:	4c 8b 95 18 ff ff ff 	mov    -0xe8(%rbp),%r10
    2b11:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    2b18:	e9 16 ec ff ff       	jmp    1733 <_dl_relocate_object+0x1433>
    2b1d:	4c 89 df             	mov    %r11,%rdi
    2b20:	4c 89 95 18 ff ff ff 	mov    %r10,-0xe8(%rbp)
    2b27:	4c 89 9d 60 ff ff ff 	mov    %r11,-0xa0(%rbp)
    2b2e:	e8 00 00 00 00       	call   2b33 <_dl_relocate_object+0x2833>	2b2f: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2b33:	4c 8b 9d 60 ff ff ff 	mov    -0xa0(%rbp),%r11
    2b3a:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2b3e:	4c 8b 95 18 ff ff ff 	mov    -0xe8(%rbp),%r10
    2b45:	49 8b 93 88 04 00 00 	mov    0x488(%r11),%rdx
    2b4c:	e9 8e eb ff ff       	jmp    16df <_dl_relocate_object+0x13df>
    2b51:	49 83 fe 24          	cmp    $0x24,%r14
    2b55:	0f 84 af f8 ff ff    	je     240a <_dl_relocate_object+0x210a>
    2b5b:	45 31 c9             	xor    %r9d,%r9d
    2b5e:	85 c0                	test   %eax,%eax
    2b60:	0f 85 ab f3 ff ff    	jne    1f11 <_dl_relocate_object+0x1c11>
    2b66:	e9 ae f8 ff ff       	jmp    2419 <_dl_relocate_object+0x2119>
    2b6b:	41 8b 95 48 04 00 00 	mov    0x448(%r13),%edx
    2b72:	85 d2                	test   %edx,%edx
    2b74:	0f 85 59 f5 ff ff    	jne    20d3 <_dl_relocate_object+0x1dd3>
    2b7a:	e9 de f5 ff ff       	jmp    215d <_dl_relocate_object+0x1e5d>
    2b7f:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    2b85:	83 f8 02             	cmp    $0x2,%eax
    2b88:	0f 85 83 f3 ff ff    	jne    1f11 <_dl_relocate_object+0x1c11>
    2b8e:	e9 86 f8 ff ff       	jmp    2419 <_dl_relocate_object+0x2119>
    2b93:	48 8b 43 10          	mov    0x10(%rbx),%rax
    2b97:	49 89 42 08          	mov    %rax,0x8(%r10)
    2b9b:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        # 2ba2 <_dl_relocate_object+0x28a2>	2b9e: R_X86_64_PC32	_dl_tlsdesc_undefweak-0x4
    2ba2:	49 89 02             	mov    %rax,(%r10)
    2ba5:	e9 56 fb ff ff       	jmp    2700 <_dl_relocate_object+0x2400>
    2baa:	45 31 c9             	xor    %r9d,%r9d
    2bad:	85 c0                	test   %eax,%eax
    2baf:	0f 85 5c f3 ff ff    	jne    1f11 <_dl_relocate_object+0x1c11>
    2bb5:	e9 5f f8 ff ff       	jmp    2419 <_dl_relocate_object+0x2119>
    2bba:	45 8b 8c 24 48 04 00 00 	mov    0x448(%r12),%r9d
    2bc2:	45 85 c9             	test   %r9d,%r9d
    2bc5:	0f 85 f7 fb ff ff    	jne    27c2 <_dl_relocate_object+0x24c2>
    2bcb:	e9 6b fc ff ff       	jmp    283b <_dl_relocate_object+0x253b>
    2bd0:	49 83 fe 24          	cmp    $0x24,%r14
    2bd4:	0f 84 d0 fe ff ff    	je     2aaa <_dl_relocate_object+0x27aa>
    2bda:	45 31 c9             	xor    %r9d,%r9d
    2bdd:	85 c0                	test   %eax,%eax
    2bdf:	0f 85 49 fa ff ff    	jne    262e <_dl_relocate_object+0x232e>
    2be5:	e9 cf fe ff ff       	jmp    2ab9 <_dl_relocate_object+0x27b9>
    2bea:	31 db                	xor    %ebx,%ebx
    2bec:	e9 77 d7 ff ff       	jmp    368 <_dl_relocate_object+0x68>
    2bf1:	48 89 d7             	mov    %rdx,%rdi
    2bf4:	4c 89 9d 08 ff ff ff 	mov    %r11,-0xf8(%rbp)
    2bfb:	4c 89 95 50 ff ff ff 	mov    %r10,-0xb0(%rbp)
    2c02:	48 89 95 60 ff ff ff 	mov    %rdx,-0xa0(%rbp)
    2c09:	e8 00 00 00 00       	call   2c0e <_dl_relocate_object+0x290e>	2c0a: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2c0e:	48 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%rdx
    2c15:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2c19:	4c 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%r10
    2c20:	4c 8b 9d 08 ff ff ff 	mov    -0xf8(%rbp),%r11
    2c27:	48 8b 8a 88 04 00 00 	mov    0x488(%rdx),%rcx
    2c2e:	e9 e2 f5 ff ff       	jmp    2215 <_dl_relocate_object+0x1f15>
    2c33:	45 31 ed             	xor    %r13d,%r13d
    2c36:	48 8d 15 00 00 00 00 	lea    0x0(%rip),%rdx        # 2c3d <_dl_relocate_object+0x293d>	2c39: R_X86_64_PC32	.LC2-0x4
    2c3d:	e9 9e ef ff ff       	jmp    1be0 <_dl_relocate_object+0x18e0>
    2c42:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 2c49 <_dl_relocate_object+0x2949>	2c45: R_X86_64_PC32	__PRETTY_FUNCTION__.2-0x4
    2c49:	ba f7 01 00 00       	mov    $0x1f7,%edx
    2c4e:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 2c55 <_dl_relocate_object+0x2955>	2c51: R_X86_64_PC32	.LC13-0x4
    2c55:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2c5c <_dl_relocate_object+0x295c>	2c58: R_X86_64_PC32	.LC14-0x4
    2c5c:	e8 00 00 00 00       	call   2c61 <_dl_relocate_object+0x2961>	2c5d: R_X86_64_PLT32	__libc_assert_fail-0x4
    2c61:	41 b9 02 00 00 00    	mov    $0x2,%r9d
    2c67:	83 f8 02             	cmp    $0x2,%eax
    2c6a:	0f 85 be f9 ff ff    	jne    262e <_dl_relocate_object+0x232e>
    2c70:	e9 44 fe ff ff       	jmp    2ab9 <_dl_relocate_object+0x27b9>
    2c75:	48 89 d7             	mov    %rdx,%rdi
    2c78:	4c 89 9d 08 ff ff ff 	mov    %r11,-0xf8(%rbp)
    2c7f:	4c 89 95 50 ff ff ff 	mov    %r10,-0xb0(%rbp)
    2c86:	48 89 95 60 ff ff ff 	mov    %rdx,-0xa0(%rbp)
    2c8d:	e8 00 00 00 00       	call   2c92 <_dl_relocate_object+0x2992>	2c8e: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2c92:	48 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%rdx
    2c99:	48 8b 45 88          	mov    -0x78(%rbp),%rax
    2c9d:	4c 8b 95 50 ff ff ff 	mov    -0xb0(%rbp),%r10
    2ca4:	4c 8b 9d 08 ff ff ff 	mov    -0xf8(%rbp),%r11
    2cab:	48 8b 8a 88 04 00 00 	mov    0x488(%rdx),%rcx
    2cb2:	e9 49 f6 ff ff       	jmp    2300 <_dl_relocate_object+0x2000>
    2cb7:	45 31 c9             	xor    %r9d,%r9d
    2cba:	85 c0                	test   %eax,%eax
    2cbc:	0f 85 6c f9 ff ff    	jne    262e <_dl_relocate_object+0x232e>
    2cc2:	e9 f2 fd ff ff       	jmp    2ab9 <_dl_relocate_object+0x27b9>
    2cc7:	48 89 c7             	mov    %rax,%rdi
    2cca:	4c 89 9d 10 ff ff ff 	mov    %r11,-0xf0(%rbp)
    2cd1:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    2cd8:	48 89 85 68 ff ff ff 	mov    %rax,-0x98(%rbp)
    2cdf:	e8 00 00 00 00       	call   2ce4 <_dl_relocate_object+0x29e4>	2ce0: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2ce4:	48 8b 85 68 ff ff ff 	mov    -0x98(%rbp),%rax
    2ceb:	48 8b 4d 88          	mov    -0x78(%rbp),%rcx
    2cef:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    2cf6:	4c 8b 9d 10 ff ff ff 	mov    -0xf0(%rbp),%r11
    2cfd:	48 8b 90 88 04 00 00 	mov    0x488(%rax),%rdx
    2d04:	e9 b6 fc ff ff       	jmp    29bf <_dl_relocate_object+0x26bf>
    2d09:	48 89 c7             	mov    %rax,%rdi
    2d0c:	4c 89 9d 10 ff ff ff 	mov    %r11,-0xf0(%rbp)
    2d13:	4c 89 95 60 ff ff ff 	mov    %r10,-0xa0(%rbp)
    2d1a:	48 89 85 68 ff ff ff 	mov    %rax,-0x98(%rbp)
    2d21:	e8 00 00 00 00       	call   2d26 <_dl_relocate_object+0x2a26>	2d22: R_X86_64_PLT32	_dl_allocate_static_tls-0x4
    2d26:	48 8b 85 68 ff ff ff 	mov    -0x98(%rbp),%rax
    2d2d:	48 8b 4d 88          	mov    -0x78(%rbp),%rcx
    2d31:	4c 8b 95 60 ff ff ff 	mov    -0xa0(%rbp),%r10
    2d38:	4c 8b 9d 10 ff ff ff 	mov    -0xf0(%rbp),%r11
    2d3f:	48 8b 90 88 04 00 00 	mov    0x488(%rax),%rdx
    2d46:	e9 c8 fc ff ff       	jmp    2a13 <_dl_relocate_object+0x2713>
    2d4b:	4d 89 e2             	mov    %r12,%r10
    2d4e:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 2d55 <_dl_relocate_object+0x2a55>	2d51: R_X86_64_PC32	.LC5-0x4
    2d55:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 2d5c <_dl_relocate_object+0x2a5c>	2d58: R_X86_64_GOTTPOFF	__libc_errno-0x4
    2d5c:	49 8b 72 08          	mov    0x8(%r10),%rsi
    2d60:	31 d2                	xor    %edx,%edx
    2d62:	64 8b 38             	mov    %fs:(%rax),%edi
    2d65:	e8 00 00 00 00       	call   2d6a <_dl_relocate_object+0x2a6a>	2d66: R_X86_64_PLT32	_dl_signal_error-0x4
    2d6a:	49 89 da             	mov    %rbx,%r10
    2d6d:	48 8d 0d 00 00 00 00 	lea    0x0(%rip),%rcx        # 2d74 <_dl_relocate_object+0x2a74>	2d70: R_X86_64_PC32	.LC4-0x4
    2d74:	eb df                	jmp    2d55 <_dl_relocate_object+0x2a55>
    2d76:	48 8b 05 00 00 00 00 	mov    0x0(%rip),%rax        # 2d7d <_dl_relocate_object+0x2a7d>	2d79: R_X86_64_PC32	_dl_argv-0x4
    2d7d:	49 8b 52 08          	mov    0x8(%r10),%rdx
    2d81:	48 8b 30             	mov    (%rax),%rsi
    2d84:	48 85 f6             	test   %rsi,%rsi
    2d87:	74 65                	je     2dee <_dl_relocate_object+0x2aee>
    2d89:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2d90 <_dl_relocate_object+0x2a90>	2d8c: R_X86_64_PC32	.LC15-0x4
    2d90:	31 c0                	xor    %eax,%eax
    2d92:	e8 00 00 00 00       	call   2d97 <_dl_relocate_object+0x2a97>	2d93: R_X86_64_PLT32	_dl_fatal_printf-0x4
    2d97:	48 85 f6             	test   %rsi,%rsi
    2d9a:	74 64                	je     2e00 <_dl_relocate_object+0x2b00>
    2d9c:	48 89 d1             	mov    %rdx,%rcx
    2d9f:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2da6 <_dl_relocate_object+0x2aa6>	2da2: R_X86_64_PC32	.LC11-0x4
    2da6:	4c 89 c2             	mov    %r8,%rdx
    2da9:	31 c0                	xor    %eax,%eax
    2dab:	e8 00 00 00 00       	call   2db0 <_dl_relocate_object+0x2ab0>	2dac: R_X86_64_PLT32	_dl_fatal_printf-0x4
    2db0:	48 89 f7             	mov    %rsi,%rdi
    2db3:	49 89 d1             	mov    %rdx,%r9
    2db6:	48 85 f6             	test   %rsi,%rsi
    2db9:	74 3c                	je     2df7 <_dl_relocate_object+0x2af7>
    2dbb:	48 89 fe             	mov    %rdi,%rsi
    2dbe:	4c 89 c9             	mov    %r9,%rcx
    2dc1:	4c 89 c2             	mov    %r8,%rdx
    2dc4:	31 c0                	xor    %eax,%eax
    2dc6:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2dcd <_dl_relocate_object+0x2acd>	2dc9: R_X86_64_PC32	.LC11-0x4
    2dcd:	e8 00 00 00 00       	call   2dd2 <_dl_relocate_object+0x2ad2>	2dce: R_X86_64_PLT32	_dl_fatal_printf-0x4
    2dd2:	49 89 d7             	mov    %rdx,%r15
    2dd5:	48 85 f6             	test   %rsi,%rsi
    2dd8:	74 2f                	je     2e09 <_dl_relocate_object+0x2b09>
    2dda:	4c 89 f9             	mov    %r15,%rcx
    2ddd:	4c 89 c2             	mov    %r8,%rdx
    2de0:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2de7 <_dl_relocate_object+0x2ae7>	2de3: R_X86_64_PC32	.LC11-0x4
    2de7:	31 c0                	xor    %eax,%eax
    2de9:	e8 00 00 00 00       	call   2dee <_dl_relocate_object+0x2aee>	2dea: R_X86_64_PLT32	_dl_fatal_printf-0x4
    2dee:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 2df5 <_dl_relocate_object+0x2af5>	2df1: R_X86_64_PC32	.LC6-0x4
    2df5:	eb 92                	jmp    2d89 <_dl_relocate_object+0x2a89>
    2df7:	48 8d 3d 00 00 00 00 	lea    0x0(%rip),%rdi        # 2dfe <_dl_relocate_object+0x2afe>	2dfa: R_X86_64_PC32	.LC6-0x4
    2dfe:	eb bb                	jmp    2dbb <_dl_relocate_object+0x2abb>
    2e00:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 2e07 <_dl_relocate_object+0x2b07>	2e03: R_X86_64_PC32	.LC6-0x4
    2e07:	eb 93                	jmp    2d9c <_dl_relocate_object+0x2a9c>
    2e09:	48 8d 35 00 00 00 00 	lea    0x0(%rip),%rsi        # 2e10 <_dl_relocate_object+0x2b10>	2e0c: R_X86_64_PC32	.LC6-0x4
    2e10:	eb c8                	jmp    2dda <_dl_relocate_object+0x2ada>
