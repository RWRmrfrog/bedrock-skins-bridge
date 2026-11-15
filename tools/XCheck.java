import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;
import java.util.zip.*;
import java.awt.image.BufferedImage;
import javax.imageio.ImageIO;

/** Reimplements BedrockSkinsNetworking's wire format with plain JDK classes (computeHash and the
 *  GZIP codec are copied verbatim from the mod) to cross-check the Rust plugin. */
public class XCheck {
    static final HexFormat HEX = HexFormat.of();
    static final byte[] EMPTY_BYTES = new byte[0];

    // --- verbatim from BedrockSkinsNetworking.computeHash ---
    public static String computeHash(String geometry, byte[] textureData, byte[] capeData) throws Exception {
        java.security.MessageDigest digest = java.security.MessageDigest.getInstance("SHA-256");
        byte[] geometryBytes = geometry == null ? EMPTY_BYTES : geometry.getBytes(StandardCharsets.UTF_8);
        digest.update(java.nio.ByteBuffer.allocate(12).putInt(geometryBytes.length)
            .putInt(textureData == null ? 0 : textureData.length)
            .putInt(capeData == null ? 0 : capeData.length).array());
        digest.update(geometryBytes);
        if (textureData != null) digest.update(textureData);
        if (capeData != null) digest.update(capeData);
        return HEX.formatHex(digest.digest());
    }

    // --- FriendlyByteBuf subset ---
    static class W { ByteArrayOutputStream o = new ByteArrayOutputStream(); DataOutputStream d = new DataOutputStream(o);
        void varInt(int v) throws IOException { while ((v & ~0x7F) != 0) { d.writeByte((v & 0x7F) | 0x80); v >>>= 7; } d.writeByte(v); }
        void bytes(byte[] b) throws IOException { varInt(b.length); d.write(b); }
        void utf(String s) throws IOException { bytes(s.getBytes(StandardCharsets.UTF_8)); }
        void uuid(UUID u) throws IOException { d.writeLong(u.getMostSignificantBits()); d.writeLong(u.getLeastSignificantBits()); }
        // verbatim logic of COMPRESSED_GEOMETRY_CODEC.encode
        void geometry(String string) throws IOException {
            if (string == null || string.isEmpty()) { bytes(new byte[0]); return; }
            byte[] geometryBytes = string.getBytes(StandardCharsets.UTF_8);
            ByteArrayOutputStream baos = new ByteArrayOutputStream(1024);
            try (GZIPOutputStream gzip = new GZIPOutputStream(baos)) { gzip.write(geometryBytes); }
            if (baos.size() > 262_144) throw new IOException("Compressed geometry too large");
            bytes(baos.toByteArray());
        }
    }
    static class R { DataInputStream d; int left;
        R(byte[] b) { d = new DataInputStream(new ByteArrayInputStream(b)); left = b.length; }
        int varInt() throws IOException { int v = 0, i = 0; byte b; do { b = d.readByte(); v |= (b & 0x7F) << (7 * i++); if (i > 5) throw new IOException("varint"); } while ((b & 0x80) != 0); return v; }
        byte[] bytes(int max) throws IOException { int n = varInt(); if (n > max) throw new IOException("too big " + n); byte[] b = new byte[n]; d.readFully(b); return b; }
        String utf(int max) throws IOException { byte[] b = bytes(max * 3); String s = new String(b, StandardCharsets.UTF_8); if (s.length() > max) throw new IOException("long"); return s; }
        UUID uuid() throws IOException { return new UUID(d.readLong(), d.readLong()); }
        String skinId() throws IOException { return d.readBoolean() ? utf(256) : null; }
        // verbatim logic of COMPRESSED_GEOMETRY_CODEC.decode
        String geometry() throws IOException {
            byte[] bytes = bytes(262_144);
            if (bytes.length == 0) return "";
            try (GZIPInputStream gzip = new GZIPInputStream(new ByteArrayInputStream(bytes))) {
                byte[] uncompressed = gzip.readNBytes(1_048_576);
                if (gzip.read() != -1) throw new IOException("Geometry JSON exceeded maximum safe length!");
                return new String(uncompressed, StandardCharsets.UTF_8);
            }
        }
        void end() throws IOException { if (d.available() != 0) throw new IOException(d.available() + " bytes left over"); }
    }

    static int pix(int x, int y) { return ((x * 4) & 0xFF) << 16 | ((y * 4) & 0xFF) << 8 | ((x ^ y) * 9 & 0xFF) | (((x + y) % 5 == 0) ? 0 : 0xFF) << 24; }
    static byte[] png(int w, int h) throws IOException {
        BufferedImage img = new BufferedImage(w, h, BufferedImage.TYPE_INT_ARGB);
        for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) img.setRGB(x, y, pix(x, y));
        ByteArrayOutputStream o = new ByteArrayOutputStream(); ImageIO.write(img, "png", o); return o.toByteArray();
    }
    static void checkPng(byte[] data, int w, int h, String what) throws IOException {
        BufferedImage img = ImageIO.read(new ByteArrayInputStream(data));
        if (img.getWidth() != w || img.getHeight() != h) throw new IOException(what + " size");
        for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
            int a = img.getRGB(x, y), e = pix(x, y);
            // fully transparent pixels may lose colour; compare alpha then colour where opaque
            if ((a >>> 24) != (e >>> 24) || ((e >>> 24) != 0 && a != e)) throw new IOException(what + " pixel " + x + "," + y + ": " + Integer.toHexString(a) + " vs " + Integer.toHexString(e));
        }
    }

    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[1]);
        if (args[0].equals("make")) {
            StringBuilder g = new StringBuilder("{\"format_version\":\"1.12.0\",\"minecraft:geometry\":[{\"description\":{\"identifier\":\"geometry.xcheck\",\"texture_width\":64,\"texture_height\":64},\"bones\":[");
            for (int i = 0; i < 400; i++) g.append(i == 0 ? "" : ",").append("{\"name\":\"b").append(i).append("\",\"pivot\":[0,").append(i % 32).append(",0],\"cubes\":[{\"origin\":[-4,").append(i).append(",-4],\"size\":[8,8,8],\"uv\":[0,0]}]}");
            
            g.append("]}],\"note\":\"\u00e9\u4e2d\ud83d\ude00\"}"); String geometry = g.toString();  // non-ASCII must hash as UTF-8
            byte[] tex = png(64, 64), cape = png(64, 32);
            W w = new W(); w.d.writeBoolean(true); w.utf("xcheck:fox"); w.geometry(geometry); w.bytes(tex); w.bytes(cape);
            Files.write(dir.resolve("java_setskin.bin"), w.o.toByteArray());
            Files.writeString(dir.resolve("java_hash.txt"), computeHash(geometry, tex, cape));
            System.out.println("java: wrote set_skin (" + w.o.size() + " bytes)");
        } else {
            // skin_update from Rust (geometry gzipped by Rust, texture PNG encoded by Rust)
            R r = new R(Files.readAllBytes(dir.resolve("rust_update.bin")));
            UUID u = r.uuid(); String id = r.skinId(); String geo = r.geometry(); byte[] tex = r.bytes(1048576); byte[] cape = r.bytes(262144); r.end();
            String hash = computeHash(geo, tex, cape);
            if (!hash.equals(Files.readString(dir.resolve("rust_hash.txt")).trim())) throw new IOException("hash mismatch " + hash);
            checkPng(tex, 64, 64, "skin"); checkPng(cape, 64, 32, "cape");
            System.out.println("java: skin_update OK uuid=" + u + " id=" + id + " geometry=" + geo.length() + " chars, hash matches");
            // skin_announce
            r = new R(Files.readAllBytes(dir.resolve("rust_announce.bin")));
            UUID au = r.uuid(); String aid = r.skinId(); String ah = r.utf(256); r.end();
            if (!ah.equals(hash) || !au.equals(u)) throw new IOException("announce mismatch");
            System.out.println("java: skin_announce OK " + aid);
            // cosmetics_update
            r = new R(Files.readAllBytes(dir.resolve("rust_cosmetics.bin")));
            r.uuid(); int n = r.varInt();
            for (int i = 0; i < n; i++) { r.utf(128); r.utf(64); r.utf(128); int z = r.varInt(); for (int k = 0; k < z; k++) r.utf(64); r.geometry(); r.geometry(); r.bytes(1048576); r.d.readInt(); r.utf(8); }
            r.end();
            System.out.println("java: cosmetics_update OK (" + n + " cosmetics)");
            // emote_update
            r = new R(Files.readAllBytes(dir.resolve("rust_emote.bin")));
            r.uuid(); String eid = r.utf(128); r.utf(128); r.utf(128); String anim = r.geometry(); float dur = r.d.readFloat(); r.end();
            System.out.println("java: emote_update OK id=" + eid + " animation=" + anim.length() + " chars duration=" + dur);
        }
    }
}
