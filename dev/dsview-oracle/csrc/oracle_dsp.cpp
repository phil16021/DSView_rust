/* The numeric kernels of pv/data/spectrumstack.cpp, for comparing
 * dsview-dsp. SpectrumStack itself needs a SigSession and DsoSignals, so
 * the body of calc_fft() (from "prepare _xn data" on) and window() are
 * copied here verbatim; only the inputs become parameters. Linked with the
 * system FFTW, as DSView is. See oracle.h. */

#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <vector>
#include <fftw3.h>

#include "libsigrok.h"

extern "C" {
#include "oracle.h"
}

#define PI 3.1415

using namespace std;

static double window(uint64_t i, int type, uint64_t _sample_num)
{
    const double n_m_1 = _sample_num-1;
    switch(type) {
    case 1: // Hann window
        return 0.5*(1-cos(2*PI*i/n_m_1));
    case 2: // Hamming window
        return 0.54-0.46*cos(2*PI*i/n_m_1);
    case 3: // Blackman window
        return 0.42659-0.49656*cos(2*PI*i/n_m_1) + 0.076849*cos(4*PI*i/n_m_1);
    case 4: // Flat_top window
        return 1-1.93*cos(2*PI*i/n_m_1)+1.29*cos(4*PI*i/n_m_1)-
                 0.388*cos(6*PI*i/n_m_1)+0.028*cos(8*PI*i/n_m_1);
    default:
        return 1;
    }
}

int osdsp_power_spectrum(const uint8_t *samples, uint64_t _sample_num, uint16_t step,
                         int offset, uint64_t vdiv, uint64_t factor, int _windows_index,
                         double *out)
{
    if (_sample_num < 2)
        return -1;
    std::vector<double> _xn(_sample_num), _xk(_sample_num), _power_spectrum(_sample_num/2+1);
    fftw_plan _fft_plan = fftw_plan_r2r_1d(_sample_num, _xn.data(), _xk.data(),
                                           FFTW_R2HC, FFTW_ESTIMATE);

    const double vscale = vdiv * factor * DS_CONF_DSO_VDIVS / (1000*255.0);
    double wsum = 0;

    for (unsigned int i = 0; i < _sample_num; i++) {
        double w = window(i, _windows_index, _sample_num);
        _xn[i] = (samples[i*step] - offset) * vscale * w;
        wsum += w;
    }

    // fft
    fftw_execute(_fft_plan);

    // calculate power spectrum
    _power_spectrum[0] = abs(_xk[0])/wsum;  /* DC component */
    for (unsigned int k = 1; k < (_sample_num + 1) / 2; ++k)  /* (k < N/2 rounded up) */
         _power_spectrum[k] = sqrt((_xk[k]*_xk[k] + _xk[_sample_num-k]*_xk[_sample_num-k]) * 2) / wsum;
    if (_sample_num % 2 == 0) /* N is even */
         _power_spectrum[_sample_num/2] = abs(_xk[_sample_num/2])/wsum;  /* Nyquist freq. */

    fftw_destroy_plan(_fft_plan);
    for (uint64_t k = 0; k < _sample_num/2+1; k++)
        out[k] = _power_spectrum[k];
    return 0;
}
