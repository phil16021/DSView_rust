/*
 * patgen-g474: a pattern generator for DSViewR hardware tests on a
 * NUCLEO-G474RE. Text commands over the ST-LINK virtual COM port
 * (LPUART1, PA2/PA3, 115200 8N1), one per line; every reply starts with
 * "ok" or "err". Continuous patterns (scan, count, glitch) run until the
 * next byte arrives, which starts the next command.
 *
 * Commands (pins are written like PA5 or PB6):
 *   id                              -> ok patgen-g474 <version> <sysclk> <hse24|hsi>
 *   idle                            peripherals off, pins outputs at their level
 *   set PIN V                       drive PIN to V (0/1), e.g. an idle level
 *   map P0 P1 P2 P3                 the pins on DSLogic channels 0..3
 *   scan                            candidate pin k toggles every (k+1)*4 us
 *   pwm HZ DUTY                     TIM2_CH1 on PA5 and TIM4_CH1 on PB6
 *   count CYCLES BITS               BITS-bit Gray counter on mapped pins 0..,
 *                                   one step every CYCLES sysclk cycles
 *   uart BAUD N CFG                 USART1_TX on PB6: bytes k & 0xff,
 *                                   k = 0..N-1; CFG 8N1, 8E1 or 8N2
 *   spi DIV MODE N                  SPI1: SCK PA5, MOSI PA7, CS PA4 (GPIO),
 *                                   2 ms after SCK takes its idle level;
 *                                   SCK = sysclk >> (DIV + 1); bytes k & 0xff
 *   bspi HALF MODE N                bit-banged SPI on the mapped pins
 *                                   (SCK ch0, CS ch1, MOSI ch2, MISO ch3)
 *   i2c HALF N                      bit-banged I2C on the mapped pins
 *                                   (SCL ch0, SDA ch1), push-pull
 *   glitch CH WIDTH GAP_US          pulses of WIDTH cycles (0 = back to back
 *                                   writes) on channel CH, every GAP_US
 *   burst CH N HALF                 N pulses on channel CH, HALF cycles high
 *                                   and low, then idle
 */
#include <stdint.h>
#include "stm32g474xx.h"

#define VERSION 1
#define SYSCLK 170000000u

/* ---------------------------------------------------------------- basics */

void *memset(void *d, int c, unsigned n)
{
    uint8_t *p = d;
    while (n--)
        *p++ = (uint8_t)c;
    return d;
}

void *memcpy(void *d, const void *s, unsigned n)
{
    uint8_t *p = d;
    const uint8_t *q = s;
    while (n--)
        *p++ = *q++;
    return d;
}

static inline uint32_t cyc(void) { return DWT->CYCCNT; }

static inline void wait_until(uint32_t t)
{
    while ((int32_t)(cyc() - t) < 0) {
    }
}

static void delay_us(uint32_t us) { wait_until(cyc() + us * (SYSCLK / 1000000u)); }

static const char *clock_src = "hsi";

static void clock_init(void)
{
    RCC->APB1ENR1 |= RCC_APB1ENR1_PWREN;
    (void)RCC->APB1ENR1;
    PWR->CR5 &= ~PWR_CR5_R1MODE; /* boost mode for 170 MHz */
    FLASH->ACR = (FLASH->ACR & ~FLASH_ACR_LATENCY) | FLASH_ACR_LATENCY_4WS | FLASH_ACR_PRFTEN |
                 FLASH_ACR_ICEN | FLASH_ACR_DCEN;
    while ((FLASH->ACR & FLASH_ACR_LATENCY) != FLASH_ACR_LATENCY_4WS) {
    }
    /* the 24 MHz crystal (X3) if it starts: 24 / 6 * 85 / 2 = 170 MHz;
       else HSI 16 MHz (+-1 %): 16 / 4 * 85 / 2 */
    RCC->CR |= RCC_CR_HSEON;
    for (uint32_t i = 0; i < 2000000u && !(RCC->CR & RCC_CR_HSERDY); i++) {
    }
    if (RCC->CR & RCC_CR_HSERDY) {
        clock_src = "hse24";
        RCC->PLLCFGR = RCC_PLLCFGR_PLLSRC_HSE | (5u << RCC_PLLCFGR_PLLM_Pos) |
                       (85u << RCC_PLLCFGR_PLLN_Pos) | RCC_PLLCFGR_PLLREN;
    } else {
        RCC->CR &= ~RCC_CR_HSEON;
        RCC->PLLCFGR = RCC_PLLCFGR_PLLSRC_HSI | (3u << RCC_PLLCFGR_PLLM_Pos) |
                       (85u << RCC_PLLCFGR_PLLN_Pos) | RCC_PLLCFGR_PLLREN;
    }
    RCC->CR |= RCC_CR_PLLON;
    while (!(RCC->CR & RCC_CR_PLLRDY)) {
    }
    /* switch through AHB/2 as the reference manual asks above 80 MHz */
    RCC->CFGR = (RCC->CFGR & ~(RCC_CFGR_HPRE | RCC_CFGR_SW)) | RCC_CFGR_HPRE_DIV2 | RCC_CFGR_SW_PLL;
    while ((RCC->CFGR & RCC_CFGR_SWS) != RCC_CFGR_SWS_PLL) {
    }
    for (volatile int i = 0; i < 200; i++) {
    }
    RCC->CFGR &= ~RCC_CFGR_HPRE;

    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CYCCNT = 0;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
}

/* ------------------------------------------------------------------ pins */

typedef struct {
    GPIO_TypeDef *port;
    uint8_t n;
} pin_t;

#define MODE_IN 0u
#define MODE_OUT 1u
#define MODE_AF 2u

static void pin_mode(pin_t p, uint32_t mode, uint32_t af)
{
    uint32_t s = p.n * 2u;
    p.port->OSPEEDR |= 3u << s;
    p.port->OTYPER &= ~(1u << p.n);
    if (mode == MODE_AF) {
        volatile uint32_t *afr = &p.port->AFR[p.n >> 3];
        uint32_t a = (p.n & 7u) * 4u;
        *afr = (*afr & ~(0xFu << a)) | (af << a);
    }
    p.port->MODER = (p.port->MODER & ~(3u << s)) | (mode << s);
}

static inline void pin_set(pin_t p, int v) { p.port->BSRR = v ? (1u << p.n) : (1u << (p.n + 16)); }

/* candidates for the scan: everything on the Arduino/morpho headers of
   ports A and B except the VCP (PA2/PA3), SWD (PA13/PA14), SWO (PB3)
   and BOOT0 (PB8) */
static const pin_t CANDIDATES[] = {
    {GPIOA, 0},  {GPIOA, 1},  {GPIOA, 4},  {GPIOA, 5},  {GPIOA, 6},  {GPIOA, 7},  {GPIOA, 8},
    {GPIOA, 9},  {GPIOA, 10}, {GPIOA, 11}, {GPIOA, 12}, {GPIOA, 15}, {GPIOB, 0},  {GPIOB, 1},
    {GPIOB, 2},  {GPIOB, 4},  {GPIOB, 5},  {GPIOB, 6},  {GPIOB, 7},  {GPIOB, 9},  {GPIOB, 10},
    {GPIOB, 11}, {GPIOB, 12}, {GPIOB, 13}, {GPIOB, 14}, {GPIOB, 15},
};
#define NCAND (sizeof CANDIDATES / sizeof CANDIDATES[0])

static pin_t chmap[4] = {{GPIOA, 5}, {GPIOA, 4}, {GPIOA, 7}, {GPIOB, 6}};

static int parse_pin(const char *s, pin_t *p)
{
    if (s[0] != 'P' && s[0] != 'p')
        return 0;
    char c = s[1] | 0x20;
    if (c != 'a' && c != 'b')
        return 0;
    unsigned n = 0;
    const char *q = s + 2;
    if (*q < '0' || *q > '9')
        return 0;
    while (*q >= '0' && *q <= '9')
        n = n * 10 + (unsigned)(*q++ - '0');
    if (*q || n > 15)
        return 0;
    p->port = c == 'a' ? GPIOA : GPIOB;
    p->n = (uint8_t)n;
    return 1;
}

/* ------------------------------------------------------------- the VCP */

static void vcp_init(void)
{
    RCC->APB1ENR2 |= RCC_APB1ENR2_LPUART1EN;
    (void)RCC->APB1ENR2;
    pin_mode((pin_t){GPIOA, 2}, MODE_AF, 12);
    pin_mode((pin_t){GPIOA, 3}, MODE_AF, 12);
    LPUART1->BRR = (uint32_t)((256ull * SYSCLK + 57600) / 115200);
    LPUART1->CR1 = USART_CR1_TE | USART_CR1_RE | USART_CR1_UE;
}

static inline int vcp_pending(void) { return (LPUART1->ISR & USART_ISR_RXNE_RXFNE) != 0; }

static void vcp_putc(char c)
{
    while (!(LPUART1->ISR & USART_ISR_TXE_TXFNF)) {
    }
    LPUART1->TDR = (uint8_t)c;
}

static void vcp_puts(const char *s)
{
    while (*s)
        vcp_putc(*s++);
}

static void vcp_putu(uint64_t v)
{
    char b[24];
    int i = 0;
    do {
        b[i++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (i)
        vcp_putc(b[--i]);
}

/* ------------------------------------------------------------ peripherals */

static void all_idle(void)
{
    TIM2->CR1 = 0;
    TIM4->CR1 = 0;
    SPI1->CR1 = 0;
    USART1->CR1 = 0;
    /* the pins keep their levels (low after a reset): a command must not
       make an edge a capture could trigger on before its data */
    for (unsigned k = 0; k < NCAND; k++)
        pin_mode(CANDIDATES[k], MODE_OUT, 0);
}

static uint32_t pwm(uint32_t hz, uint32_t duty)
{
    uint32_t period = SYSCLK / hz;
    if (period < 2)
        period = 2;
    uint32_t psc = (period - 1) / 65536u;
    uint32_t arr = period / (psc + 1) - 1;
    uint32_t ccr = (uint32_t)((uint64_t)(arr + 1) * duty / 100u);
    TIM_TypeDef *t[2] = {TIM2, TIM4};
    for (int i = 0; i < 2; i++) {
        t[i]->CR1 = 0;
        t[i]->PSC = psc;
        t[i]->ARR = arr;
        t[i]->CCR1 = ccr;
        t[i]->CCMR1 = (6u << TIM_CCMR1_OC1M_Pos) | TIM_CCMR1_OC1PE;
        t[i]->CCER = TIM_CCER_CC1E;
        t[i]->CNT = 0;
        t[i]->EGR = TIM_EGR_UG;
    }
    pin_mode((pin_t){GPIOA, 5}, MODE_AF, 1);
    pin_mode((pin_t){GPIOB, 6}, MODE_AF, 2);
    TIM2->CR1 = TIM_CR1_ARPE | TIM_CR1_CEN;
    TIM4->CR1 = TIM_CR1_ARPE | TIM_CR1_CEN;
    return (psc + 1) * (arr + 1); /* the period in sysclk cycles */
}

/* Waits for a VCP byte; returns 1 if one arrived. */
static inline int stop_requested(void) { return vcp_pending(); }

static void scan(void)
{
    uint32_t cnt[NCAND];
    uint32_t state = 0;
    for (unsigned k = 0; k < NCAND; k++)
        cnt[k] = k + 1;
    const uint32_t tick = 4u * (SYSCLK / 1000000u);
    uint32_t t = cyc() + tick;
    while (!stop_requested()) {
        uint32_t a_set = 0, a_rst = 0, b_set = 0, b_rst = 0;
        for (unsigned k = 0; k < NCAND; k++) {
            if (--cnt[k] == 0) {
                cnt[k] = k + 1;
                state ^= 1u << k;
                uint32_t m = 1u << CANDIDATES[k].n;
                int hi = (state >> k) & 1u;
                if (CANDIDATES[k].port == GPIOA) {
                    if (hi)
                        a_set |= m;
                    else
                        a_rst |= m;
                } else {
                    if (hi)
                        b_set |= m;
                    else
                        b_rst |= m;
                }
            }
        }
        wait_until(t);
        GPIOA->BSRR = a_set | (a_rst << 16);
        GPIOB->BSRR = b_set | (b_rst << 16);
        t += tick;
    }
}

static void count(uint32_t step, uint32_t bits)
{
    uint32_t idx = 0, g = 0, mask = (1u << bits) - 1;
    for (int i = 0; i < 4; i++)
        pin_set(chmap[i], 0);
    uint32_t t = cyc() + step;
    while (!stop_requested()) {
        idx = (idx + 1) & mask;
        uint32_t ng = idx ^ (idx >> 1);
        uint32_t d = ng ^ g;
        int b = d & 1 ? 0 : d & 2 ? 1 : d & 4 ? 2 : 3;
        g = ng;
        wait_until(t);
        pin_set(chmap[b], (int)((g >> b) & 1u));
        t += step;
    }
}

static int uart(uint32_t baud, uint32_t n, const char *cfg)
{
    uint32_t cr1 = USART_CR1_TE;
    uint32_t cr2 = 0;
    if (cfg[0] != '8' || !cfg[1] || !cfg[2] || cfg[3])
        return 0;
    if (cfg[1] == 'E')
        cr1 |= USART_CR1_M0 | USART_CR1_PCE;
    else if (cfg[1] == 'O')
        cr1 |= USART_CR1_M0 | USART_CR1_PCE | USART_CR1_PS;
    else if (cfg[1] != 'N')
        return 0;
    if (cfg[2] == '2')
        cr2 |= 2u << USART_CR2_STOP_Pos;
    else if (cfg[2] != '1')
        return 0;
    RCC->APB2ENR |= RCC_APB2ENR_USART1EN;
    (void)RCC->APB2ENR;
    USART1->CR1 = 0;
    USART1->BRR = (SYSCLK + baud / 2) / baud;
    USART1->CR2 = cr2;
    USART1->CR1 = cr1;
    USART1->CR1 = cr1 | USART_CR1_UE;
    /* the line idles high for 2 ms before the first start bit */
    pin_set((pin_t){GPIOB, 6}, 1);
    pin_mode((pin_t){GPIOB, 6}, MODE_AF, 7);
    delay_us(2000);
    for (uint32_t k = 0; k < n; k++) {
        while (!(USART1->ISR & USART_ISR_TXE_TXFNF)) {
        }
        USART1->TDR = k & 0xffu;
    }
    while (!(USART1->ISR & USART_ISR_TC)) {
    }
    return (int)USART1->BRR;
}

static void spi(uint32_t div, uint32_t mode, uint32_t n)
{
    RCC->APB2ENR |= RCC_APB2ENR_SPI1EN;
    (void)RCC->APB2ENR;
    SPI1->CR1 = 0;
    pin_t cs = {GPIOA, 4};
    pin_set(cs, 1);
    pin_mode(cs, MODE_OUT, 0);
    /* enabled first: SCK sits at CPOL before the pin is handed over */
    SPI1->CR2 = (7u << SPI_CR2_DS_Pos) | SPI_CR2_FRXTH;
    SPI1->CR1 = SPI_CR1_MSTR | SPI_CR1_SSM | SPI_CR1_SSI | ((div & 7u) << SPI_CR1_BR_Pos) |
                (mode & 3u);
    SPI1->CR1 |= SPI_CR1_SPE;
    pin_mode((pin_t){GPIOA, 5}, MODE_AF, 5);
    pin_mode((pin_t){GPIOA, 6}, MODE_AF, 5);
    pin_mode((pin_t){GPIOA, 7}, MODE_AF, 5);
    delay_us(2000); /* SCK settles at its idle level well before the data */
    pin_set(cs, 0);
    delay_us(2);
    for (uint32_t k = 0; k < n; k++) {
        while (!(SPI1->SR & SPI_SR_TXE)) {
        }
        *(volatile uint8_t *)&SPI1->DR = (uint8_t)k;
        while (SPI1->SR & SPI_SR_RXNE)
            (void)*(volatile uint8_t *)&SPI1->DR;
    }
    while (SPI1->SR & (SPI_SR_BSY | SPI_SR_FTLVL)) {
        while (SPI1->SR & SPI_SR_RXNE)
            (void)*(volatile uint8_t *)&SPI1->DR;
    }
    delay_us(2);
    pin_set(cs, 1);
    while (SPI1->SR & SPI_SR_RXNE)
        (void)*(volatile uint8_t *)&SPI1->DR;
    /* SCK stays at its idle level when the SPI lets go of it */
    pin_set((pin_t){GPIOA, 5}, (int)((mode >> 1) & 1u));
    pin_mode((pin_t){GPIOA, 5}, MODE_OUT, 0);
    SPI1->CR1 = 0;
}

/* bit-banged SPI: SCK ch0, CS ch1, MOSI ch2, MISO ch3 (a copy of MOSI
   inverted, so the decoder has a second data line) */
static void bspi(uint32_t half, uint32_t mode, uint32_t n)
{
    pin_t sck = chmap[0], cs = chmap[1], mosi = chmap[2], miso = chmap[3];
    int cpol = (mode >> 1) & 1, cpha = mode & 1;
    pin_set(sck, cpol);
    pin_set(cs, 1);
    pin_set(mosi, 0);
    pin_set(miso, 1);
    for (int i = 0; i < 4; i++)
        pin_mode(chmap[i], MODE_OUT, 0);
    delay_us(10);
    uint32_t t = cyc() + half;
    pin_set(cs, 0);
    t += half;
    for (uint32_t k = 0; k < n; k++) {
        uint8_t v = (uint8_t)k;
        for (int b = 7; b >= 0; b--) {
            int bit = (v >> b) & 1;
            if (!cpha) {
                wait_until(t);
                pin_set(mosi, bit);
                pin_set(miso, !bit);
                t += half;
                wait_until(t);
                pin_set(sck, !cpol);
                t += half;
                wait_until(t);
                pin_set(sck, cpol);
            } else {
                wait_until(t);
                pin_set(sck, !cpol);
                pin_set(mosi, bit);
                pin_set(miso, !bit);
                t += half;
                wait_until(t);
                pin_set(sck, cpol);
                t += half;
            }
        }
        t += half;
    }
    t += half;
    wait_until(t);
    pin_set(cs, 1);
}

/* bit-banged I2C, push-pull: SCL ch0, SDA ch1. A write of N bytes k & 0xff
   to 0x50 (all ACKed), a repeated start, a read of 0xA5 (ACK) and 0x5A
   (NACK) from 0x50, then a stop. */
static uint32_t i2c_t, i2c_half;
static void i2c_step(void)
{
    i2c_t += i2c_half;
    wait_until(i2c_t);
}

static void i2c_bit(int v)
{
    pin_set(chmap[1], v);
    i2c_step();
    pin_set(chmap[0], 1);
    i2c_step();
    pin_set(chmap[0], 0);
}

static void i2c_byte(uint8_t v, int ack)
{
    for (int b = 7; b >= 0; b--)
        i2c_bit((v >> b) & 1);
    i2c_bit(!ack);
}

static void i2c(uint32_t half, uint32_t n)
{
    pin_t scl = chmap[0], sda = chmap[1];
    pin_set(scl, 1);
    pin_set(sda, 1);
    pin_mode(scl, MODE_OUT, 0);
    pin_mode(sda, MODE_OUT, 0);
    delay_us(10);
    i2c_half = half;
    i2c_t = cyc();
    pin_set(sda, 0); /* start */
    i2c_step();
    pin_set(scl, 0);
    i2c_byte(0x50 << 1, 1);
    for (uint32_t k = 0; k < n; k++)
        i2c_byte((uint8_t)k, 1);
    pin_set(sda, 1); /* repeated start */
    i2c_step();
    pin_set(scl, 1);
    i2c_step();
    pin_set(sda, 0);
    i2c_step();
    pin_set(scl, 0);
    i2c_byte((0x50 << 1) | 1, 1);
    i2c_byte(0xA5, 1);
    i2c_byte(0x5A, 0);
    pin_set(sda, 0); /* stop */
    i2c_step();
    pin_set(scl, 1);
    i2c_step();
    pin_set(sda, 1);
    i2c_step();
}

static void glitch(uint32_t ch, uint32_t width, uint32_t gap_us)
{
    pin_t p = chmap[ch & 3u];
    volatile uint32_t *bsrr = &p.port->BSRR;
    uint32_t on = 1u << p.n, off = 1u << (p.n + 16);
    pin_set(p, 0);
    uint32_t gap = gap_us * (SYSCLK / 1000000u);
    uint32_t t = cyc() + gap;
    while (!stop_requested()) {
        wait_until(t);
        __disable_irq();
        if (width == 0) {
            *bsrr = on;
            *bsrr = off;
        } else {
            uint32_t s = cyc();
            *bsrr = on;
            wait_until(s + width);
            *bsrr = off;
        }
        __enable_irq();
        t += gap;
    }
}

static void burst(uint32_t ch, uint32_t n, uint32_t half)
{
    pin_t p = chmap[ch & 3u];
    pin_set(p, 0);
    uint32_t t = cyc() + half;
    for (uint32_t k = 0; k < n; k++) {
        wait_until(t);
        pin_set(p, 1);
        t += half;
        wait_until(t);
        pin_set(p, 0);
        t += half;
    }
}

/* -------------------------------------------------------------- commands */

static char line[96];
static unsigned line_len;

static int split(char *s, char **argv, int max)
{
    int n = 0;
    while (*s && n < max) {
        while (*s == ' ')
            *s++ = 0;
        if (!*s)
            break;
        argv[n++] = s;
        while (*s && *s != ' ')
            s++;
    }
    return n;
}

static int num(const char *s, uint32_t *v)
{
    uint32_t r = 0;
    if (!*s)
        return 0;
    while (*s) {
        if (*s < '0' || *s > '9')
            return 0;
        r = r * 10 + (uint32_t)(*s++ - '0');
    }
    *v = r;
    return 1;
}

static int streq(const char *a, const char *b)
{
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}

static void put_pin(pin_t p)
{
    vcp_putc('P');
    vcp_putc(p.port == GPIOA ? 'A' : 'B');
    vcp_putu(p.n);
}

static void run(char *s)
{
    char *a[6];
    uint32_t v[5] = {0};
    int n = split(s, a, 6);
    if (!n)
        return;
    for (int i = 1; i < n && i < 6; i++)
        if (!num(a[i], &v[i - 1]))
            v[i - 1] = 0xFFFFFFFFu;
#define ARGS(k)                                                                                    \
    do {                                                                                           \
        if (n != (k) + 1) {                                                                        \
            vcp_puts("err usage\n");                                                               \
            return;                                                                                \
        }                                                                                          \
    } while (0)
    if (streq(a[0], "id")) {
        vcp_puts("ok patgen-g474 ");
        vcp_putu(VERSION);
        vcp_putc(' ');
        vcp_putu(SYSCLK);
        vcp_putc(' ');
        vcp_puts(clock_src);
        vcp_putc('\n');
    } else if (streq(a[0], "idle")) {
        all_idle();
        vcp_puts("ok\n");
    } else if (streq(a[0], "map")) {
        pin_t m[4];
        if (n != 5) {
            vcp_puts("err usage\n");
            return;
        }
        for (int i = 0; i < 4; i++)
            if (!parse_pin(a[i + 1], &m[i])) {
                vcp_puts("err pin\n");
                return;
            }
        for (int i = 0; i < 4; i++)
            chmap[i] = m[i];
        vcp_puts("ok");
        for (int i = 0; i < 4; i++) {
            vcp_putc(' ');
            put_pin(chmap[i]);
        }
        vcp_putc('\n');
    } else if (streq(a[0], "set")) {
        pin_t p;
        if (n != 3 || !parse_pin(a[1], &p) || v[1] > 1) {
            vcp_puts("err usage\n");
            return;
        }
        all_idle();
        pin_set(p, (int)v[1]);
        vcp_puts("ok\n");
    } else if (streq(a[0], "scan")) {
        ARGS(0);
        all_idle();
        vcp_puts("ok tick_us 4");
        for (unsigned k = 0; k < NCAND; k++) {
            vcp_putc(' ');
            put_pin(CANDIDATES[k]);
        }
        vcp_putc('\n');
        scan();
    } else if (streq(a[0], "pwm")) {
        ARGS(2);
        if (!v[0] || v[0] > SYSCLK / 2 || v[1] > 100) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        uint32_t period = pwm(v[0], v[1]);
        vcp_puts("ok period_cycles ");
        vcp_putu(period);
        vcp_putc('\n');
    } else if (streq(a[0], "count")) {
        ARGS(2);
        if (v[0] < 40 || v[0] == 0xFFFFFFFFu || v[1] < 1 || v[1] > 4) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        vcp_puts("ok\n");
        count(v[0], v[1]);
    } else if (streq(a[0], "uart")) {
        ARGS(3);
        if (!v[0] || v[0] > SYSCLK / 16 || v[1] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        int brr = uart(v[0], v[1], a[3]);
        if (!brr) {
            vcp_puts("err cfg\n");
            return;
        }
        vcp_puts("ok brr ");
        vcp_putu((uint32_t)brr);
        vcp_putc('\n');
    } else if (streq(a[0], "spi")) {
        ARGS(3);
        if (v[0] > 7 || v[1] > 3 || v[2] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        spi(v[0], v[1], v[2]);
        vcp_puts("ok sck_hz ");
        vcp_putu(SYSCLK >> (v[0] + 1));
        vcp_putc('\n');
    } else if (streq(a[0], "bspi")) {
        ARGS(3);
        if (v[0] < 30 || v[0] == 0xFFFFFFFFu || v[1] > 3 || v[2] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        bspi(v[0], v[1], v[2]);
        vcp_puts("ok\n");
    } else if (streq(a[0], "i2c")) {
        ARGS(2);
        if (v[0] < 30 || v[0] == 0xFFFFFFFFu || v[1] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        i2c(v[0], v[1]);
        vcp_puts("ok\n");
    } else if (streq(a[0], "glitch")) {
        ARGS(3);
        if (v[0] > 3 || v[1] > 100000 || !v[2] || v[2] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        vcp_puts("ok\n");
        glitch(v[0], v[1], v[2]);
    } else if (streq(a[0], "burst")) {
        ARGS(3);
        if (v[0] > 3 || v[1] == 0xFFFFFFFFu || v[2] < 20 || v[2] == 0xFFFFFFFFu) {
            vcp_puts("err range\n");
            return;
        }
        all_idle();
        burst(v[0], v[1], v[2]);
        vcp_puts("ok\n");
    } else {
        vcp_puts("err unknown\n");
    }
}

int main(void)
{
    clock_init();
    RCC->AHB2ENR |= RCC_AHB2ENR_GPIOAEN | RCC_AHB2ENR_GPIOBEN;
    RCC->APB1ENR1 |= RCC_APB1ENR1_TIM2EN | RCC_APB1ENR1_TIM4EN;
    RCC->APB2ENR |= RCC_APB2ENR_SPI1EN | RCC_APB2ENR_USART1EN;
    (void)RCC->APB2ENR;
    vcp_init();
    all_idle();
    vcp_puts("\nok patgen-g474 ready\n");
    for (;;) {
        if (!vcp_pending())
            continue;
        char c = (char)LPUART1->RDR;
        if (c == '\r' || c == '\n') {
            line[line_len] = 0;
            run(line);
            line_len = 0;
        } else if (line_len < sizeof line - 1) {
            line[line_len++] = c;
        }
    }
}

/* -------------------------------------------------------------- startup */

extern uint32_t _estack, _sidata, _sdata, _edata, _sbss, _ebss;

void Reset_Handler(void)
{
    uint32_t *s = &_sidata, *d = &_sdata;
    while (d < &_edata)
        *d++ = *s++;
    for (d = &_sbss; d < &_ebss;)
        *d++ = 0;
    main();
    for (;;) {
    }
}

void Default_Handler(void)
{
    for (;;) {
    }
}

__attribute__((section(".isr_vector"), used)) static void (*const vectors[16])(void) = {
    (void (*)(void))&_estack,
    Reset_Handler,
    Default_Handler, /* NMI */
    Default_Handler, /* HardFault */
    Default_Handler, Default_Handler, Default_Handler, 0, 0, 0, 0,
    Default_Handler, /* SVC */
    Default_Handler, 0,
    Default_Handler, /* PendSV */
    Default_Handler, /* SysTick */
};
