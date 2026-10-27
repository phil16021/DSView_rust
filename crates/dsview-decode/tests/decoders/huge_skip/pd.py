import sigrokdecode as srd

class Decoder(srd.Decoder):
    api_version = 3
    id = 'huge_skip'
    name = 'Huge skip'
    longname = 'Skip count past 64 bits'
    desc = 'Test decoder for a skip count that does not fit 64 bits.'
    license = 'gplv2+'
    inputs = ['logic']
    outputs = []
    tags = ['Util']
    channels = (
        {'id': 'data', 'name': 'Data', 'desc': 'Data line', 'idn': 'dec_huge_skip_chan_data'},
    )
    annotations = (
        ('survived', 'wait() returned normally'),
    )

    def reset(self):
        pass

    def start(self):
        self.out_ann = self.register(srd.OUTPUT_ANN)

    def decode(self):
        self.wait({0: 'r'})
        self.wait([{'skip': 1 << 65}, {0: 'f'}])
        self.put(self.samplenum, self.samplenum, self.out_ann, [0, ['survived']])
