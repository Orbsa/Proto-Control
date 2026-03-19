/*
 * TeamSpeak 3 Client Plugin SDK
 * Copyright (c) TeamSpeak Systems GmbH
 * Obtained via ACRE2 (https://github.com/IDI-Systems/acre2) — GPLv3
 */
#ifndef TEAMLOG_LOGTYPES_H
#define TEAMLOG_LOGTYPES_H

enum LogTypes {
    LogType_NONE          = 0x0000,
    LogType_FILE          = 0x0001,
    LogType_CONSOLE       = 0x0002,
    LogType_USERLOGGING   = 0x0004,
    LogType_NO_NETLOGGING = 0x0008,
    LogType_DATABASE      = 0x0010,
    LogType_SYSLOG        = 0x0020,
};

enum LogLevel {
    LogLevel_CRITICAL = 0,
    LogLevel_ERROR,
    LogLevel_WARNING,
    LogLevel_DEBUG,
    LogLevel_INFO,
    LogLevel_DEVEL
};

#endif //TEAMLOG_LOGTYPES_H
