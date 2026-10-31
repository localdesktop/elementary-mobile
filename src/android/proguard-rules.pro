-keep class app.polarbear.KeyboardShortcutsPrompt {
    public static void showIfNeeded(android.app.Activity);
}

-keep class app.polarbear.ExternalDisplay {
    public static void start(android.app.Activity);
    public static void background(android.app.Activity);
    public static void showKeyboard(android.app.Activity);
    public static void stop(android.app.Activity);
    native <methods>;
}

-keep class app.polarbear.TrackpadControls {
    native <methods>;
}

-keep class app.polarbear.PhoneKeyboard {
    native <methods>;
}
