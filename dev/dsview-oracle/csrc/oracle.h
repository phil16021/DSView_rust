/* Interface between the Rust oracle binaries and the original C libraries. */

#ifndef DSVIEW_ORACLE_H
#define DSVIEW_ORACLE_H

#include <stdint.h>

/* ------------------------------------------------------------ decode */

struct oracle_opt {
    const char *key;
    const char *str;   /* JSON string value, or "" */
    double num;        /* JSON number value */
    int is_num;        /* 1 when the JSON value was a number */
};

struct oracle_dec {
    const char *id;
    const struct oracle_opt *opts;
    int n_opts;
    const char *const *ch_id;  /* decoder channel ids ... */
    const int *ch_idx;         /* ... bound to these signal indexes */
    int n_ch;
};

typedef void (*oracle_emit)(void *user, const char *json_line);

int oracle_decode_init(const char *decoder_dir, int log_level);
void oracle_decode_exit(void);

/* signals[i]: bit-packed LSB-first samples of signal i (NULL if absent),
 * laid out like LogicSnapshot leaves (2^24 samples per leaf from sample 0).
 * max_chunk 0 = DecoderStack::MaxChunkSize. */
int oracle_decode_run(const struct oracle_dec *stack, int n_dec, uint64_t samplerate,
                      uint64_t total_samples, const uint8_t *const *signals, int n_signals,
                      uint64_t max_chunk, oracle_emit emit, void *user);

/* ------------------------------------------------------------ .dsl loading */

#define ORACLE_MAX_CHANNELS 64

struct oracle_channel {
    int index;
    int type;      /* enum oracle_channel_kind */
    int enabled;
    char name[64];
};

struct oracle_dsl_info {
    int mode;      /* LOGIC=0, DSO=1, ANALOG=2 */
    uint64_t samplerate;
    uint64_t limit_samples;
    int n_channels;
    struct oracle_channel channels[ORACLE_MAX_CHANNELS];
};

/* Packet kinds forwarded from the libsigrok datafeed (mapped from SR_DF_*
 * on the C side so the Rust side never mirrors enum values). */
enum oracle_packet_kind {
    ORACLE_PKT_OTHER = 0,
    ORACLE_PKT_LOGIC = 1,     /* format: 0 = LA_CROSS_DATA */
    ORACLE_PKT_DSO = 2,
    ORACLE_PKT_ANALOG = 3,
    ORACLE_PKT_END = 4,
};

/* Channel kinds (mapped from SR_CHANNEL_*). */
enum oracle_channel_kind {
    ORACLE_CH_OTHER = 0,
    ORACLE_CH_LOGIC = 1,
    ORACLE_CH_DSO = 2,
    ORACLE_CH_ANALOG = 3,
};

typedef void (*oracle_packet_cb)(void *user, int type, int format, const uint8_t *data,
                                 uint64_t len, int num_samples);

/* Loads a .dsl/.demo through libsigrok4DSL's session driver (the code path
 * DSView uses for File > Open) and forwards every datafeed packet. */
int oracle_dsl_run(const char *path, int log_level, struct oracle_dsl_info *info,
                   oracle_packet_cb cb, void *user);

#endif
