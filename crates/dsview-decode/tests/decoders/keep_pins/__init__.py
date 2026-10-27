'''
Test decoder: keeps the tuple wait() returned, as pjdl and ieee488 do; the
next wait() must still work (DEVIATIONS F13).
'''

from .pd import Decoder
