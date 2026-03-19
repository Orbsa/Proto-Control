/*
 * TeamSpeak 3 Client Plugin SDK
 * Copyright (c) TeamSpeak Systems GmbH
 * Obtained via ACRE2 (https://github.com/IDI-Systems/acre2) — GPLv3
 */
#ifndef PLUGIN_DEFINITIONS
#define PLUGIN_DEFINITIONS

/* Return values for ts3plugin_offersConfigure */
enum PluginConfigureOffer {
    PLUGIN_OFFERS_NO_CONFIGURE = 0,
    PLUGIN_OFFERS_CONFIGURE_NEW_THREAD,
    PLUGIN_OFFERS_CONFIGURE_QT_THREAD
};

enum PluginMessageTarget {
    PLUGIN_MESSAGE_TARGET_SERVER = 0,
    PLUGIN_MESSAGE_TARGET_CHANNEL
};

enum PluginItemType {
    PLUGIN_SERVER = 0,
    PLUGIN_CHANNEL,
    PLUGIN_CLIENT
};

enum PluginMenuType {
    PLUGIN_MENU_TYPE_GLOBAL = 0,
    PLUGIN_MENU_TYPE_CHANNEL,
    PLUGIN_MENU_TYPE_CLIENT
};

#define PLUGIN_MENU_BUFSZ 128

struct PluginMenuItem {
    enum PluginMenuType type;
    int id;
    char text[PLUGIN_MENU_BUFSZ];
    char icon[PLUGIN_MENU_BUFSZ];
};

#define PLUGIN_HOTKEY_BUFSZ 128

struct PluginHotkey {
    char keyword[PLUGIN_HOTKEY_BUFSZ];
    char description[PLUGIN_HOTKEY_BUFSZ];
};

struct PluginBookmarkList;
struct PluginBookmarkItem {
    char*         name;
    unsigned char isFolder;
    unsigned char reserved[3];
    union{
        char*         uuid;
        struct PluginBookmarkList* folder;
    };
};

struct PluginBookmarkList{
    int itemcount;
    struct PluginBookmarkItem items[1];
};

enum PluginGuiProfile{
    PLUGIN_GUI_SOUND_CAPTURE = 0,
    PLUGIN_GUI_SOUND_PLAYBACK,
    PLUGIN_GUI_HOTKEY,
    PLUGIN_GUI_SOUNDPACK,
    PLUGIN_GUI_IDENTITY
};

enum PluginConnectTab{
    PLUGIN_CONNECT_TAB_NEW = 0,
    PLUGIN_CONNECT_TAB_CURRENT,
    PLUGIN_CONNECT_TAB_NEW_IF_CURRENT_CONNECTED
};

#endif
