// SPDX-License-Identifier: AGPL-3.0-or-later
import java.nio.file.Files;
import java.nio.file.Path;
import jdk.internal.org.objectweb.asm.Attribute;
import jdk.internal.org.objectweb.asm.ClassReader;
import jdk.internal.org.objectweb.asm.ClassVisitor;
import jdk.internal.org.objectweb.asm.ClassWriter;
import jdk.internal.org.objectweb.asm.Opcodes;

public final class ModuleHashes {
    public static void main(String[] args) throws Exception {
        Path path = Path.of(args[0]);
        ClassReader reader = new ClassReader(Files.readAllBytes(path));
        ClassWriter writer = new ClassWriter(reader, 0);
        reader.accept(new ClassVisitor(Opcodes.ASM8, writer) {
            @Override
            public void visitAttribute(Attribute attribute) {
                if (!attribute.type.equals("ModuleHashes")) {
                    super.visitAttribute(attribute);
                }
            }
        }, 0);
        Files.write(path, writer.toByteArray());
    }
}
