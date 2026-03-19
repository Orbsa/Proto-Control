CC      := gcc
CFLAGS  := -Wall -Wextra -O2 -fPIC -fvisibility=hidden -D_GNU_SOURCE -DBUILDING_DLL
INC     := -Iinc
LDFLAGS := -shared -pthread
OUT     := protocontrol_ts3_linux_amd64.so
SRC     := protocontrol_ts3.c

.PHONY: all clean install

all: $(OUT)

$(OUT): $(SRC)
	$(CC) $(CFLAGS) $(INC) $(LDFLAGS) -o $@ $<
	patchelf --remove-rpath $@

TS3_PLUGINS := $(HOME)/.var/app/com.teamspeak.TeamSpeak3/.ts3client/plugins
install: $(OUT)
	install -d $(TS3_PLUGINS)
	install -m 644 $(OUT) $(TS3_PLUGINS)/

clean:
	rm -f $(OUT)
