# JNA reflects native methods, structures and callback interfaces.
-keep class com.sun.jna.** { *; }
# JNA's optional desktop AWT helpers are not used on Android.
-dontwarn java.awt.Component
-dontwarn java.awt.GraphicsEnvironment
-dontwarn java.awt.HeadlessException
-dontwarn java.awt.Window
-keepattributes RuntimeVisibleAnnotations
-keep class app.rufin.rufin.** extends com.sun.jna.Structure { *; }
-keep interface app.rufin.rufin.** extends com.sun.jna.Callback { *; }
-keepclassmembers class app.rufin.rufin.** implements com.sun.jna.Callback {
    public *;
}

# These helpers are constructed or called by name from native code.
-keep class app.rufin.rufin.platform.AndroidAudioTrack { *; }
-keep class app.rufin.rufin.platform.AndroidNetwork { *; }
-keep class app.rufin.rufin.platform.AndroidDiscoveryClient { *; }
-keep class app.rufin.rufin.platform.AndroidDocuments { *; }
-keep class app.rufin.rufin.platform.AndroidKeyring { *; }
-keep class org.freedesktop.gstreamer.** { *; }
-keep class org.rustls.platformverifier.** { *; }
