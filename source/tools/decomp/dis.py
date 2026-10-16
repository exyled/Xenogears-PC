import struct, sys
names = ['zero','at','v0','v1','a0','a1','a2','a3','t0','t1','t2','t3','t4','t5','t6','t7','s0','s1','s2','s3','s4','s5','s6','s7','t8','t9','k0','k1','gp','sp','s8','ra']
def dis(a, w):
    op=w>>26; rs=(w>>21)&31; rt=(w>>16)&31; rd=(w>>11)&31; sh=(w>>6)&31; fn=w&63
    imm=w&0xFFFF; simm=imm-0x10000 if imm&0x8000 else imm
    R=lambda i:'$'+names[i]
    if w==0: return 'nop'
    if op==0:
        m={0:'sll',2:'srl',3:'sra',4:'sllv',6:'srlv',7:'srav',8:'jr',9:'jalr',12:'syscall',13:'break',16:'mfhi',17:'mthi',18:'mflo',19:'mtlo',24:'mult',25:'multu',26:'div',27:'divu',32:'add',33:'addu',34:'sub',35:'subu',36:'and',37:'or',38:'xor',39:'nor',42:'slt',43:'sltu'}
        n=m.get(fn,'special%d'%fn)
        if fn in (0,2,3): return '%s %s,%s,%d'%(n,R(rd),R(rt),sh)
        if fn==8: return 'jr %s'%R(rs)
        if fn==9: return 'jalr %s,%s'%(R(rd),R(rs))
        if fn in (16,18): return '%s %s'%(n,R(rd))
        if fn in (17,19): return '%s %s'%(n,R(rs))
        if fn in (24,25,26,27): return '%s %s,%s'%(n,R(rs),R(rt))
        if fn in (12,13): return n
        return '%s %s,%s,%s'%(n,R(rd),R(rs),R(rt))
    if op==1: return '%s %s,0x%08X'%('bgez' if rt&1 else 'bltz',R(rs),(a+4+simm*4)&0xFFFFFFFF)
    if op==2: return 'j 0x%08X'%(((a&0xF0000000)|((w&0x3FFFFFF)<<2)))
    if op==3: return 'jal 0x%08X'%(((a&0xF0000000)|((w&0x3FFFFFF)<<2)))
    if op in (4,5): return '%s %s,%s,0x%08X'%('beq' if op==4 else 'bne',R(rs),R(rt),(a+4+simm*4)&0xFFFFFFFF)
    if op in (6,7): return '%s %s,0x%08X'%('blez' if op==6 else 'bgtz',R(rs),(a+4+simm*4)&0xFFFFFFFF)
    m={8:'addi',9:'addiu',10:'slti',11:'sltiu',12:'andi',13:'ori',14:'xori'}
    if op in m: return '%s %s,%s,%d'%(m[op],R(rt),R(rs),simm if op in (8,9,10,11) else imm)
    if op==15: return 'lui %s,0x%04X'%(R(rt),imm)
    m={32:'lb',33:'lh',34:'lwl',35:'lw',36:'lbu',37:'lhu',38:'lwr',40:'sb',41:'sh',42:'swl',43:'sw',46:'swr',50:'lwc2',58:'swc2'}
    if op in m: return '%s %s,%d(%s)'%(m[op],R(rt),simm,R(rs))
    if op==16: return 'cop0 %08X'%w
    if op==18: return 'cop2 %08X'%w
    return 'op%d %08X'%(op,w)
if __name__ == '__main__':
    from discover import load_image
    import json, os
    rows = json.load(open(os.path.join(os.path.dirname(__file__), '..', 'overlays.json')))
    base, words = load_image(rows[int(sys.argv[1])])
    a = int(sys.argv[2], 16); n = int(sys.argv[3])
    for k in range(n):
        print('%08X  %s' % (a + 4*k, dis(a + 4*k, words[(a - base) >> 2 + 0 if False else ((a - base) >> 2) + k])))
