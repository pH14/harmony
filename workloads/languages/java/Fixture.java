// SPDX-License-Identifier: AGPL-3.0-or-later
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.concurrent.CountDownLatch;

public final class Fixture {
    private static volatile boolean done;

    private static final long CODE_CACHE_LIMIT = 240L << 20;

    private static void checkMappings() throws IOException {
        if (!System.getProperty("java.vm.name").contains("Server")) {
            throw new AssertionError("expected the server VM, found " + System.getProperty("java.vm.name"));
        }
        long low = Long.MAX_VALUE;
        long high = 0;
        for (String line : Files.readAllLines(Path.of("/proc/self/maps"))) {
            String[] fields = line.trim().split("\\s+", 6);
            if (fields[1].indexOf('x') < 0) {
                continue;
            }
            String name = fields.length == 6 ? fields[5] : "";
            if (name.equals("[vdso]") || name.equals("[vsyscall]")) {
                continue;
            }
            if (name.startsWith("/")) {
                if (fields[1].indexOf('w') >= 0) {
                    throw new AssertionError("writable executable file mapping: " + line);
                }
                continue;
            }
            String[] range = fields[0].split("-");
            low = Math.min(low, Long.parseUnsignedLong(range[0], 16));
            high = Math.max(high, Long.parseUnsignedLong(range[1], 16));
        }
        if (high > low && high - low > CODE_CACHE_LIMIT) {
            throw new AssertionError("anonymous executable mappings span more than the code cache");
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
