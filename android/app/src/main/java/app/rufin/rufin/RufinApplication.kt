package app.rufin.rufin

import android.app.Application
import android.os.Process
import android.system.Os

internal class RufinApplication : Application() {
    init {
        // The GStreamer 1.28 SDK's OpenSSL 3.5.0 assembles armeabi-v7a aes_v8_set_encrypt_key
        // without loading its round constants (fixed in 3.5.1, openssl b7f6519229), so ARMv8
        // AES crashes HTTPS. Its CPU probe runs when any Rufin library first loads; keep it to
        // NEON.
        if (!Process.is64Bit()) Os.setenv("OPENSSL_armcap", "1", true)
    }
}
