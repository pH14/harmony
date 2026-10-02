// SPDX-License-Identifier: AGPL-3.0-or-later
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.stream.Stream;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassVisitor;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Label;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;

public final class CoverageRewriter {
    private static final String TICK_OWNER = "java/lang/HarmonyCoverage";
    private static long sites;

    public static void main(String[] args) throws IOException {
        if (args.length == 0) {
            throw new IllegalArgumentException("usage: CoverageRewriter CLASS-DIRECTORY...");
        }
        long classes = 0;
        for (String argument : args) {
            List<Path> files;
            try (Stream<Path> walk = Files.walk(Path.of(argument))) {
                files = walk.filter(path -> path.toString().endsWith(".class")).sorted().toList();
            }
            for (Path file : files) {
                byte[] original = Files.readAllBytes(file);
                byte[] rewritten = rewrite(original);
                if (rewritten != original) {
                    Files.write(file, rewritten);
                    classes++;
                }
            }
        }
        System.out.printf("rewrote %d classes, %d back-edges%n", classes, sites);
    }

    static byte[] rewrite(byte[] original) {
        ClassReader reader = new ClassReader(original);
        String name = reader.getClassName();
        if (name.equals("module-info") || name.equals(TICK_OWNER)) {
            return original;
        }
        long before = sites;
        ClassWriter writer = new ClassWriter(reader, 0);
        reader.accept(new ClassVisitor(Opcodes.ASM9, writer) {
            @Override
            public MethodVisitor visitMethod(int access, String method, String descriptor, String signature,
                    String[] exceptions) {
                return new BackEdges(super.visitMethod(access, method, descriptor, signature, exceptions));
            }
        }, 0);
        return sites == before ? original : writer.toByteArray();
    }

    private static final class BackEdges extends MethodVisitor {
        private final Set<Label> seen = new HashSet<>();

        BackEdges(MethodVisitor next) {
            super(Opcodes.ASM9, next);
        }

        private void tick() {
            sites++;
            super.visitMethodInsn(Opcodes.INVOKESTATIC, TICK_OWNER, "tick", "()V", false);
        }

        @Override
        public void visitLabel(Label label) {
            seen.add(label);
            super.visitLabel(label);
        }

        @Override
        public void visitJumpInsn(int opcode, Label label) {
            if (seen.contains(label)) {
                tick();
            }
            super.visitJumpInsn(opcode, label);
        }

        @Override
        public void visitTableSwitchInsn(int min, int max, Label fallback, Label... labels) {
            if (backward(fallback, labels)) {
                tick();
            }
            super.visitTableSwitchInsn(min, max, fallback, labels);
        }

        @Override
        public void visitLookupSwitchInsn(Label fallback, int[] keys, Label[] labels) {
            if (backward(fallback, labels)) {
                tick();
            }
            super.visitLookupSwitchInsn(fallback, keys, labels);
        }

        private boolean backward(Label fallback, Label[] labels) {
            if (seen.contains(fallback)) {
                return true;
            }
            for (Label label : labels) {
                if (seen.contains(label)) {
                    return true;
                }
            }
            return false;
        }
    }
}
