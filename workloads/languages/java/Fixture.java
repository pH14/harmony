// SPDX-License-Identifier: AGPL-3.0-or-later
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.concurrent.CountDownLatch;

public final class Fixture {
    private static volatile boolean done;

    private static void checkMappings() throws IOException {
        if (!System.getProperty("java.vm.name").contains("Zero")) {
            throw new AssertionError("expected the Zero VM, found " + System.getProperty("java.vm.name"));
        }
        for (String line : Files.readAllLines(Path.of("/proc/self/maps"))) {
            String[] fields = line.trim().split("\\s+", 6);
            if (fields[1].indexOf('x') < 0) {
                continue;
            }
            boolean named = fields.length == 6
                    && (fields[5].startsWith("/") || fields[5].equals("[vdso]") || fields[5].equals("[vsyscall]"));
            if (fields[1].indexOf('w') >= 0 || !named) {
                throw new AssertionError("executable anonymous or writable mapping: " + line);
            }
        }
    }

    public static void main(String[] args) throws Exception {
        checkMappings();
        CountDownLatch started = new CountDownLatch(1);
        Thread spinner = new Thread(() -> {
            started.countDown();
            while (!done) {
            }
        });
        spinner.start();
        started.await();
        System.out.println("HARMONY_LANGUAGE_READY");
        System.out.flush();
        for (int marker = 1; marker <= 20; marker++) {
            Thread.sleep(10);
            System.out.printf("HARMONY_LANGUAGE_MARKER %02d%n", marker);
            System.out.flush();
        }
        done = true;
        spinner.join();
        checkMappings();
    }
}
